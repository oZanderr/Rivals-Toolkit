import React, {
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { useVirtualizer } from "@tanstack/react-virtual";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  AlertTriangle,
  ArrowLeft,
  ArrowLeftRight,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Database,
  Eraser,
  Eye,
  EyeOff,
  Flag,
  Minus,
  Pencil,
  Plus,
  Loader2,
  MoreHorizontal,
  Package,
  RefreshCw,
  RotateCcw,
  Save,
  Table2,
  Trash2,
  Undo2,
  X,
} from "lucide-react";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tip } from "@/components/ui/tooltip";
import {
  bulkTarget,
  draftKey,
  scriptTarget,
  EditSessionContext,
  elementTarget,
  entryTarget,
  isKeyDraft,
  isStructural,
  payloadTarget,
  rowTarget,
  SaveTarget,
  stringTarget,
  useAssetEdits,
  useEditSession,
  type AssetEdits,
  type Draft,
  type DraftRecord,
  type EditSession,
  type EditTarget,
  type Structural,
  withWarnings,
} from "@/hooks/useAssetEdits";
import { useExportClipboard, type ExportClipboard } from "@/hooks/useExportClipboard";
import { useSaveHotkeys } from "@/hooks/useSaveHotkeys";
import { previewContainerFilename } from "@/lib/pakName";
import { cn } from "@/lib/utils";

export type PropertyValue =
  | { kind: "bool"; value: boolean }
  | { kind: "int"; value: number }
  | { kind: "uint"; value: number }
  | { kind: "float"; value: number }
  | { kind: "byte"; value: number }
  | { kind: "str"; value: string }
  | { kind: "name"; value: string }
  | { kind: "text"; value?: string; parts?: PropertyEntry[] }
  | { kind: "enum"; value: number; name?: string; enum_type?: string }
  | { kind: "object"; index: number; path?: string }
  | { kind: "soft_object"; path: string }
  | { kind: "delegate"; object?: string; function: string }
  | { kind: "field_path"; path: string }
  | { kind: "lazy_object"; guid: string }
  | { kind: "array"; items: PropertyValue[] }
  | { kind: "set"; items: PropertyValue[] }
  | { kind: "map"; entries: { key: PropertyValue; value: PropertyValue }[] }
  | { kind: "struct"; name: string; fields: PropertyEntry[] }
  | { kind: "undecoded"; reason: string; bytes: number }
  | { kind: "default"; declared?: string; fields?: PropertyEntry[] }
  | { kind: "unset"; declared: string; enum_type?: string; fields?: PropertyEntry[] };

export interface PropertyEntry {
  name: string;
  element?: number;
  value: PropertyValue;
  /// Byte range of the value. An empty range means the header flagged it as holding its
  /// default, so it is written nowhere and that offset is where storing it would put it.
  span?: [number, number];
}

interface DataTable {
  row_struct: string;
  columns: string[];
  rows: { name: string; fields: PropertyEntry[] }[];
  declared_rows: number;
  truncated?: string;
}

type ExportStatus =
  | { state: "complete" }
  | { state: "payload"; consumed: number; payload_bytes: number; kind: string }
  | { state: "partial"; consumed: number; expected: number }
  | { state: "failed"; reason: string };

interface ParsedExport {
  index: number;
  object_name: string;
  class_name: string;
  serial_offset: number;
  serial_size: number;
  /// Raw FPackageIndex values: negative for an import, positive for an export, zero for none.
  outer_index: number;
  class_index: number;
  super_index: number;
  template_index: number;
  object_flags: number;
  generate_public_hash: boolean;
  path: string;
  status: ExportStatus;
  properties: PropertyEntry[];
  data_table?: DataTable;
  string_table?: StringTable;
  trailing_hex?: string;
  /** Why a class, function or struct layout walk stopped short, when it did. */
  note?: string;
  /** Instanced struct payloads inside this export that did not decode. The export can still read
   *  as exact, since each payload's length prefix puts the cursor back. */
  undecoded?: UndecodedPayload[];
  /** The bytecode this export stores, when it has any. */
  script?: { buffer_size: number; storage_size: number; decoded_size: number };
}

/** The four preload dependency runs, as the backend serializes them. */
interface DependencyRuns {
  serialize_before_serialize: number[];
  create_before_serialize: number[];
  serialize_before_create: number[];
  create_before_create: number[];
}

/** A payload the reader could not decode, kept as its bytes and named so it is not silent. */
interface UndecodedPayload {
  struct_name: string;
  at: number;
  end: number;
  reason: string;
}

/** The entries a UStringTable writes after its properties. */
interface StringTable {
  namespace: string;
  entries: { key: string; source: string; tag: string; metadata: [string, string][] }[];
}

/** What `plan_export_removal` reports, for the user to weigh before anything is written. */
interface RemovalPlan {
  removed: { index: number; path: string; class_name: string; requested: boolean }[];
  blockers: string[];
  cleared: { export: number; export_name: string; property: string; target: string }[];
  warnings: string[];
  renumbered: number;
  public: string[];
  importers: { path: string; packages: string[] }[];
  index_available: boolean;
}

/** What `plan_export_edits` reports: the paths a table edit moves, and who follows them by hash. */
interface ExportEditPlan {
  blockers: string[];
  warnings: string[];
  public: string[];
  repathed: [string, string][];
  importers: { path: string; packages: string[] }[];
  index_available: boolean;
}

/** What `plan_import_removal` reports before an import row is dropped. */
interface ImportRemovalPlan {
  removed: { index: number; path: string; class_name: string }[];
  blockers: string[];
  cleared: string[];
  warnings: string[];
  renumbered: number;
  dropped_dependencies: number;
}

/** A change to a header table awaiting the user's confirmation. */
type StructuralAsk =
  | { kind: "remove"; index: number; plan: RemovalPlan | null; error: string | null }
  | { kind: "reset"; index: number }
  | { kind: "duplicate"; index: number }
  | {
      kind: "rename";
      index: number;
      name: string;
      plan: ExportEditPlan | null;
      error: string | null;
    }
  | { kind: "flags"; index: number; set: number; clear: number }
  | {
      kind: "retype";
      index: number;
      class: number | null;
      plan: ExportEditPlan | null;
      error: string | null;
    }
  | {
      kind: "deps";
      index: number;
      runs: DependencyRuns;
      plan: DependencyPlan | null;
      error: string | null;
    }
  | {
      kind: "drop_import";
      import: number;
      plan: ImportRemovalPlan | null;
      error: string | null;
    }
  | {
      kind: "paste";
      held: ExportClipboard;
      /** The export the copy goes under, or null for the package root. */
      outer: number | null;
      name: string;
      plan: CopyPlan | null;
      error: string | null;
    };

/** What `plan_export_copy` reports before an export is brought in from another package. */
interface CopyPlan {
  blockers: string[];
  warnings: string[];
  copies: {
    from: string;
    export: number;
    path: string;
    class_name: string;
    index: number;
    requested: boolean;
  }[];
}

/** What `plan_dependency_edits` reports before the load order is changed. */
interface DependencyPlan {
  blockers: string[];
  warnings: string[];
  cycle?: string[];
}

/** What names an import, which is what decides whether it can be dropped. */
interface ImportUsage {
  references: number;
  roles: string[];
  outer_of: number[];
  preload: number;
  resources: number;
}

interface ImportInfo {
  index: number;
  class_package: string;
  class_name: string;
  outer_index: number;
  object_name: string;
  path: string;
  unresolved: boolean;
  usage: ImportUsage;
}

/** Whether nothing in the package names this import. */
function importUnused(usage: ImportUsage): boolean {
  return (
    usage.references === 0 &&
    usage.roles.length === 0 &&
    usage.outer_of.length === 0 &&
    usage.preload === 0 &&
    usage.resources === 0
  );
}

/** What holds an import in the table, in the order the dialog reads best. */
function importUsageText(usage: ImportUsage): string {
  const held: string[] = [];
  if (usage.references > 0)
    held.push(`${usage.references} reference${usage.references === 1 ? "" : "s"}`);
  held.push(...usage.roles);
  if (usage.outer_of.length > 0) held.push(`outer of ${usage.outer_of.length}`);
  if (usage.preload > 0) held.push(`${usage.preload} dependencies`);
  if (usage.resources > 0) held.push(`${usage.resources} bulk resources`);
  return held.length === 0 ? "named by nothing" : held.join(", ");
}

interface ParsedPackage {
  package_name: string;
  cooked: boolean;
  unversioned_properties: boolean;
  name_count: number;
  import_count: number;
  export_count: number;
  names: string[];
  imports: ImportInfo[];
  exports: ParsedExport[];
  /** The runs each export declares, in export order. Absent when the table did not read. */
  dependencies?: DependencyRuns[];
  unresolved_structs: string[];
  schema_fixups?: AppliedFixup[];
  resources: ResourceInfo[];
}

/** One bulk data resource: where its payload sits and whether the bytes can be replaced. */
interface ResourceInfo {
  index: number;
  serial_offset: number;
  serial_size: number;
  raw_size: number;
  flags: number;
  placement: string;
  owner?: number;
  locked?: string;
}

/** Why an export's payload cannot be replaced, mirroring the backend's refusals. */
function payloadLockOf(exp: ParsedExport, pkg: ParsedPackage | null): string | null {
  if (exp.status.state !== "payload") {
    return "This export carries no payload the reader measured after its properties.";
  }
  if (exp.status.kind.endsWith("layout and bytecode")) {
    return "The layout walk stopped before this bytecode, so it has no known start.";
  }
  if (pkg?.resources.some((r) => r.owner === exp.index)) {
    return "This export holds inline bulk data inside its payload; replace that through the bulk data table.";
  }
  return null;
}

interface AppliedFixup {
  struct_name: string;
  slot: number;
  property: string;
}

interface MappingsStatus {
  path: string | null;
  loaded: boolean;
  struct_count: number;
  enum_count: number;
  error: string | null;
}

type ViewMode = "table" | "strings" | "tree" | "json" | "bytes" | "script";

const INHERITED_KEY = "assetInspector.showInherited";

function readFlag(key: string): boolean {
  try {
    return localStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}

function writeFlag(key: string, value: boolean): void {
  try {
    localStorage.setItem(key, value ? "1" : "0");
  } catch {
    // Storage can be unavailable; the toggle then lasts for the session only.
  }
}

interface Props {
  gamePath: string;
  container: string;
  entry: string;
  gameRunning: boolean;
  /** Tabs stay mounted while hidden, and the save hotkeys are window-wide. */
  isActive: boolean;
  onClose: () => void;
  onOpenSettings: () => void;
  /** Opens the same entry from the mod pak a save wrote, so later edits layer on the saved copy. */
  onOpenCopy?: (container: string) => void;
  /** An export to open on arrival, by its index, and the statement in its script to land on. */
  initialTarget?: { exportIndex: number; offset?: number };
}

/** Containers hold aggregates; everything else reads on one line. */
function isExpandable(value: PropertyValue): boolean {
  switch (value.kind) {
    case "struct":
      return value.fields.length > 0;
    case "array":
    case "set":
      return value.items.length > 0;
    case "map":
      return value.entries.length > 0;
    case "text":
      return (value.parts?.length ?? 0) > 0;
    case "unset":
    case "default":
      return (value.fields?.length ?? 0) > 0;
    default:
      return false;
  }
}

function summarise(value: PropertyValue, depth = 0): string {
  switch (value.kind) {
    case "bool":
      return value.value ? "true" : "false";
    case "int":
    case "uint":
    case "byte":
      return String(value.value);
    case "float":
      return Number.isInteger(value.value) ? value.value.toFixed(1) : String(value.value);
    case "str":
    case "name":
      return value.value;
    case "text":
      return value.value ?? "";
    case "enum":
      return value.name ?? String(value.value);
    case "object":
      return value.path ?? (value.index === 0 ? "None" : String(value.index));
    case "soft_object":
      return value.path;
    case "delegate":
      return value.object ? `${value.object}::${value.function}` : value.function;
    case "field_path":
      return value.path;
    case "lazy_object":
      return value.guid;
    case "array":
      return previewList(value.items, "[", "]", depth);
    case "set":
      return previewList(value.items, "{", "}", depth);
    case "map":
      return previewList(
        value.entries.map((e) => e.value),
        "{",
        "}",
        depth,
        value.entries.map((e) => summariseScalar(e.key))
      );
    case "struct":
      return previewStruct(value.name, value.fields, depth);
    case "undecoded":
      return `(${value.bytes} bytes not decoded)`;
    case "default":
      return "(default)";
    case "unset":
      return "(not stored)";
  }
}

/** Scalars render inline; anything with children returns null so the caller can decide. */
function summariseScalar(value: PropertyValue): string | null {
  switch (value.kind) {
    case "array":
    case "set":
    case "map":
    case "struct":
      return null;
    default:
      return summarise(value);
  }
}

const PREVIEW_ITEMS = 8;
const PREVIEW_CHARS = 140;
const PREVIEW_DEPTH = 3;

function plural(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

/** Showing the first few values beats "3 items", which tells the reader nothing. Nested values are
 *  included while the line stays short enough to read, and give way to a count when it does not. */
function previewList(
  items: PropertyValue[],
  open: string,
  close: string,
  depth: number,
  keys?: (string | null)[]
): string {
  if (items.length === 0) return `${open}${close}`;
  if (depth >= PREVIEW_DEPTH) return plural(items.length, "item");
  const parts: string[] = [];
  for (let i = 0; i < Math.min(items.length, PREVIEW_ITEMS); i++) {
    const text = summariseScalar(items[i]) ?? summarise(items[i], depth + 1);
    const key = keys?.[i];
    parts.push(key ? `${key}: ${text}` : text);
  }
  const more = items.length > PREVIEW_ITEMS ? `, +${items.length - PREVIEW_ITEMS}` : "";
  const body = `${parts.join(", ")}${more}`;
  return body.length > PREVIEW_CHARS ? plural(items.length, "item") : `${open}${body}${close}`;
}

function previewStruct(name: string, fields: PropertyEntry[], depth: number): string {
  if (fields.length === 0) return name;
  if (depth >= PREVIEW_DEPTH) return `${name} {${plural(fields.length, "field")}}`;
  const parts: string[] = [];
  for (const field of fields.slice(0, 4)) {
    // An inherited field keeps its position but not the words; the row says the rest.
    const text = field.value.kind === "unset" ? "–" : summariseScalar(field.value);
    if (text === null) return `${name} {${plural(fields.length, "field")}}`;
    parts.push(text);
  }
  const more = fields.length > 4 ? ", ..." : "";
  const body = `${parts.join(", ")}${more}`;
  return body.length > PREVIEW_CHARS
    ? `${name} {${plural(fields.length, "field")}}`
    : `${name}(${body})`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} bytes`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

/** Mirrors PropertyEntry::label in the Rust crate: static array elements carry their index, so
 *  the columns of a table with a `float Foo[8]` property stay distinct. */
function labelOf(entry: PropertyEntry): string {
  return entry.element === undefined ? entry.name : `${entry.name}[${entry.element}]`;
}

/** Drops the slots an export does not store, at every depth, so what remains is what the file
 *  holds. Offsets and names are untouched, so edits keyed on them still resolve. */
function withoutInherited(entries: PropertyEntry[]): PropertyEntry[] {
  const kept: PropertyEntry[] = [];
  for (const entry of entries) {
    if (entry.value.kind === "unset") continue;
    kept.push({ ...entry, value: pruneValue(entry.value) });
  }
  return kept;
}

function pruneValue(value: PropertyValue): PropertyValue {
  switch (value.kind) {
    case "struct":
      return { ...value, fields: withoutInherited(value.fields) };
    case "array":
    case "set":
      return { ...value, items: value.items.map(pruneValue) };
    case "map":
      return {
        ...value,
        entries: value.entries.map((pair) => ({
          key: pruneValue(pair.key),
          value: pruneValue(pair.value),
        })),
      };
    default:
      return value;
  }
}

/** Why an export cannot be reset, or null when it can. */
function resetLockOf(exp: ParsedExport, lock: string | null): string | null {
  if (lock) return lock;
  if (exp.data_table)
    return "A DataTable's RowStruct is one of its properties; edit the rows instead.";
  if (exp.status.state === "failed") return "This export did not decode.";
  if (exp.properties.every((p) => p.value.kind === "unset")) {
    return "This export already inherits every value.";
  }
  return null;
}

/**
 * One row of the tree: the entry, and where an edit to it would land. `target` is null for a row
 * the reader recorded no bytes for, such as the wrapper around a table row, or that cannot be
 * written on its own yet; `reason` says which.
 */
interface TreeRow {
  entry: PropertyEntry;
  target: EditTarget | null;
  reason: string | null;
  /** Keys of the containers this row sits inside, outermost first. */
  within: string[];
}

const NO_POSITION = "This value has no recorded position in the file.";

function rowOf(entry: PropertyEntry, within: string[]): TreeRow {
  const target = entryTarget(entry);
  return { entry, target, reason: target ? null : NO_POSITION, within };
}

function childrenOf(row: TreeRow): TreeRow[] {
  const { entry, target, within } = row;
  const value = entry.value;
  const inner = target ? [...within, draftKey(target)] : within;
  switch (value.kind) {
    case "struct":
      return value.fields.map((field) => rowOf(field, inner));
    case "array":
    case "set":
      return value.items.map((item, i) => {
        const element = target ? elementTarget(entry, i) : null;
        return {
          entry: { name: `[${i}]`, value: item },
          target: element,
          reason: element ? null : NO_POSITION,
          within: inner,
        };
      });
    case "map":
      return value.entries.map((pair, i) => {
        const element = target ? elementTarget(entry, i) : null;
        return {
          entry: { name: `[${i}] ${summarise(pair.key)}`, value: pair.value },
          target: element,
          reason: element ? null : NO_POSITION,
          within: inner,
        };
      });
    case "text":
      return (value.parts ?? []).map((part) => rowOf(part, inner));
    // The fields of an unset or zero struct have no bytes yet, so each is reached through the
    // struct: a value typed for one stores the struct and sets it in the same save.
    case "unset":
    case "default":
      return (value.fields ?? []).map((field) => {
        const segment =
          field.element === undefined ? field.name : `${field.name}[${field.element}]`;
        const through: EditTarget | null = target
          ? { ...target, path: [...(target.path ?? []), segment], was: undefined }
          : null;
        return {
          entry: field,
          target: through,
          reason: through ? null : NO_POSITION,
          within: inner,
        };
      });
    default:
      return [];
  }
}

/** Why nothing about a row may change, or null: the export is locked, or a pending add/drop on a
 *  container above it would move its bytes. */
function structuralLock(row: TreeRow, session: EditSession): string | null {
  if (session.locked) return session.locked;
  const above = row.within.find((key) => {
    const held = session.drafts[key];
    return held !== undefined && isStructural(held.draft);
  });
  if (above) {
    return `Finish or discard the add/drop on ${session.drafts[above].target.name} first.`;
  }
  return null;
}

/** Why a row cannot be typed into, or null when it can. Checked from the outside in. */
function rowLock(row: TreeRow, session: EditSession): string | null {
  const blocked = structuralLock(row, session);
  if (blocked) return blocked;
  const value = row.entry.value;
  if (value.kind === "unset") return row.target ? unsetReason(value.declared) : row.reason;
  // A zero value is written from nothing like an unset one, when its type can be typed.
  if (value.kind === "default" && value.declared && TYPEABLE_DECLARED.has(value.declared)) {
    return row.target ? null : row.reason;
  }
  if (value.kind === "undecoded") {
    return "This payload did not decode, so its bytes are kept exactly as they are.";
  }
  if (value.kind === "struct") return "Edit the fields inside.";
  if (!row.target) return row.reason;
  if (CONTAINER_KINDS.has(value.kind)) return "Right click to add or drop an element.";
  if (!EDITABLE_KINDS.has(value.kind)) return `${value.kind} values cannot be edited yet.`;
  return null;
}

/** Whether any draft sits inside the container with this key. */
function hasDraftsWithin(session: EditSession, key: string): boolean {
  return Object.values(session.drafts).some((record) => record.within.includes(key));
}

/**
 * One context menu serves the whole tree. A tree can hold thousands of rows, and a menu root per
 * row would double what each one renders; the row that was right-clicked is remembered instead.
 */
interface TreeMenu {
  editingKey: string | null;
  setEditingKey: (key: string | null) => void;
  openMenu: (row: TreeRow) => void;
}

const TreeMenuContext = createContext<TreeMenu | null>(null);

/** A set or a map about to grow: the key form needs to know where the element goes. */
interface KeyAsk {
  target: EditTarget;
  within: string[];
  index: number;
  label: string;
  kind: "set" | "map";
  /** The keys are structs, which have no text form: the new element takes the default key. */
  structKey: boolean;
}

/** Whether a set's elements or a map's keys are structs, judged from the first one held. */
function keysAreStructs(value: PropertyValue): boolean {
  if (value.kind === "set") return value.items[0]?.kind === "struct";
  if (value.kind === "map") return value.entries[0]?.key.kind === "struct";
  return false;
}

/** Asks for the key of a new set or map element. Blank is allowed only on an empty container,
 *  which then takes the key type's default, and on a struct-keyed one, which can take nothing
 *  else. */
function KeyForm({ ask, onConfirm }: { ask: KeyAsk; onConfirm: (key: string) => void }) {
  const [key, setKey] = useState("");
  const trimmed = key.trim();
  const valid = trimmed.length > 0 || ask.index === 0 || ask.structKey;
  if (ask.structKey) {
    return (
      <>
        <AlertDialogHeader>
          <AlertDialogTitle>Add an element to {ask.label}</AlertDialogTitle>
          <AlertDialogDescription>
            This {ask.kind} keys on a struct, which has no text form. The new element takes the key
            type's default, so edit its fields once it is in; a second default element is refused
            until the first one changes.
            {ask.kind === "map" ? " Its value starts as the type's default." : ""}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={() => onConfirm("")}>Add default element</AlertDialogAction>
        </AlertDialogFooter>
      </>
    );
  }
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Add an element to {ask.label}</AlertDialogTitle>
        <AlertDialogDescription>
          A {ask.kind} keys on its contents, so the new element needs a key no other element
          carries: a name, a number, a string or an object path, as the key type takes it.
          {ask.kind === "map" ? " Its value starts as the type's default." : ""}
          {ask.index === 0 ? " Leave it blank to take the key type's default." : ""}
        </AlertDialogDescription>
      </AlertDialogHeader>
      <Input
        autoFocus
        value={key}
        onChange={(e) => setKey(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && valid) {
            e.preventDefault();
            onConfirm(trimmed);
          }
        }}
        placeholder="Key"
        className="h-8 font-mono text-xs"
      />
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction disabled={!valid} onClick={() => onConfirm(trimmed)}>
          Add
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

/** A MovieScene channel row: its keys are two bulk arrays the key dialog edits together. */
function isChannel(entry: PropertyEntry): boolean {
  return (
    entry.value.kind === "struct" &&
    (entry.value.name === "MovieSceneFloatChannel" ||
      entry.value.name === "MovieSceneDoubleChannel")
  );
}

interface KeysAsk {
  target: EditTarget;
  within: string[];
  entry: PropertyEntry;
}

/** How many key changes are queued on a channel, for the row and its menu. */
function pendingKeyCount(session: EditSession, target: EditTarget | null): number {
  if (!target) return 0;
  return Object.values(session.drafts).filter(
    (record) =>
      record.target.offset === target.offset &&
      record.target.name === target.name &&
      record.target.element === target.element &&
      isKeyDraft(record.draft)
  ).length;
}

/** The keys of one channel: frame and value per key, with a copy or a removal per key and a form
 *  for a new one. Each change is a draft of its own, so several can go in one save. */
function KeysForm({ ask, session }: { ask: KeysAsk; session: EditSession }) {
  const [time, setTime] = useState("");
  const [value, setValue] = useState("");
  const [copyFrom, setCopyFrom] = useState<number | null>(null);
  const [copyTime, setCopyTime] = useState("");
  const [moveFrom, setMoveFrom] = useState<number | null>(null);
  const [moveTime, setMoveTime] = useState("");
  const keys = useMemo(() => {
    if (ask.entry.value.kind !== "struct") return [];
    return ask.entry.value.fields
      .filter((f) => f.name === "Keys" && f.value.kind === "struct")
      .map((f, index) => {
        const fields = f.value.kind === "struct" ? f.value.fields : [];
        const frame = fields[0]?.value.kind === "int" ? fields[0].value.value : 0;
        const held = fields[1]?.value.kind === "float" ? fields[1].value.value : 0;
        return { index, frame, value: held };
      });
  }, [ask.entry]);
  const draftAt = (field: string) =>
    session.drafts[draftKey({ ...ask.target, field })]?.draft ?? null;
  const setKeyDraft = (field: string, draft: Draft) =>
    session.setDraft({ ...ask.target, field }, draft, ask.within);
  const dropKeyDraft = (field: string) => session.dropDraft(draftKey({ ...ask.target, field }));
  const frames = new Set(keys.map((key) => key.frame));
  const added = Object.values(session.drafts).filter(
    (record) =>
      record.target.offset === ask.target.offset &&
      record.target.name === ask.target.name &&
      record.target.element === ask.target.element &&
      (record.draft.op === "key_add" ||
        record.draft.op === "key_duplicate" ||
        record.draft.op === "key_move")
  );
  for (const record of added) {
    if ("time" in record.draft) frames.add(record.draft.time);
  }
  const parsedTime = Number.parseInt(time.trim(), 10);
  const parsedValue = Number(value.trim());
  const canAdd =
    Number.isInteger(parsedTime) &&
    value.trim() !== "" &&
    Number.isFinite(parsedValue) &&
    !frames.has(parsedTime) &&
    !session.locked;
  const parsedCopyTime = Number.parseInt(copyTime.trim(), 10);
  const canCopy =
    copyFrom !== null && Number.isInteger(parsedCopyTime) && !frames.has(parsedCopyTime);
  const parsedMoveTime = Number.parseInt(moveTime.trim(), 10);
  const canMove =
    moveFrom !== null && Number.isInteger(parsedMoveTime) && !frames.has(parsedMoveTime);
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Keys of {displayName(ask.entry.name)}</AlertDialogTitle>
        <AlertDialogDescription>
          Frames and values of this channel. A new key takes the tangents and modes of the key
          before it. Changes queue as drafts and land when you save.
        </AlertDialogDescription>
      </AlertDialogHeader>
      <div className="flex max-h-72 flex-col gap-1 overflow-auto text-[11px]">
        {keys.length === 0 && <p className="text-muted-foreground">This channel has no keys.</p>}
        {keys.map((key) => {
          const removal = draftAt(`key:rm:${key.index}`);
          const copy = draftAt(`key:dup:${key.index}`);
          const move = draftAt(`key:mv:${key.index}`);
          const queued = removal
            ? `key:rm:${key.index}`
            : copy
              ? `key:dup:${key.index}`
              : move
                ? `key:mv:${key.index}`
                : null;
          return (
            <div key={key.index} className="flex items-center gap-2">
              <span className="w-8 shrink-0 font-mono text-muted-foreground">{key.index}</span>
              <span
                className={cn(
                  "w-20 shrink-0 font-mono",
                  (removal || move) && "line-through opacity-60"
                )}
              >
                {key.frame}
              </span>
              <span
                className={cn(
                  "min-w-0 flex-1 truncate font-mono",
                  removal && "line-through opacity-60"
                )}
              >
                {key.value}
              </span>
              {copy?.op === "key_duplicate" && (
                <span className="font-mono text-blue-accent-foreground">copy at {copy.time}</span>
              )}
              {move?.op === "key_move" && (
                <span className="font-mono text-blue-accent-foreground">to frame {move.time}</span>
              )}
              {queued ? (
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 shrink-0"
                  onClick={() => dropKeyDraft(queued)}
                >
                  <Undo2 size={13} />
                </Button>
              ) : (
                <>
                  <Tip content="Copy this key to another frame" side="top">
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-7 shrink-0"
                      disabled={!!session.locked}
                      onClick={() => {
                        setCopyFrom(key.index);
                        setCopyTime("");
                        setMoveFrom(null);
                      }}
                    >
                      <Plus size={13} />
                    </Button>
                  </Tip>
                  <Tip
                    content="Move this key to another frame, value and tangents with it"
                    side="top"
                  >
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-7 shrink-0"
                      disabled={!!session.locked}
                      onClick={() => {
                        setMoveFrom(key.index);
                        setMoveTime("");
                        setCopyFrom(null);
                      }}
                    >
                      <ArrowLeftRight size={13} />
                    </Button>
                  </Tip>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 shrink-0 text-destructive"
                    disabled={!!session.locked}
                    onClick={() =>
                      setKeyDraft(`key:rm:${key.index}`, { op: "key_remove", index: key.index })
                    }
                  >
                    <Trash2 size={13} />
                  </Button>
                </>
              )}
            </div>
          );
        })}
        {added
          .filter((record) => record.draft.op === "key_add")
          .map((record) =>
            record.draft.op === "key_add" ? (
              <div
                key={draftKey(record.target)}
                className="flex items-center gap-2 text-blue-accent-foreground"
              >
                <span className="w-8 shrink-0 font-mono">
                  <Plus size={11} />
                </span>
                <span className="w-20 shrink-0 font-mono">{record.draft.time}</span>
                <span className="min-w-0 flex-1 truncate font-mono">{record.draft.value}</span>
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 shrink-0"
                  onClick={() => session.dropDraft(draftKey(record.target))}
                >
                  <Undo2 size={13} />
                </Button>
              </div>
            ) : null
          )}
      </div>
      {copyFrom !== null && (
        <div className="flex items-center gap-2 border-t border-border pt-2 text-[11px]">
          <span className="shrink-0 text-muted-foreground">Copy key {copyFrom} to frame</span>
          <Input
            autoFocus
            value={copyTime}
            onChange={(e) => setCopyTime(e.target.value)}
            placeholder="Frame"
            className="h-7 w-28 font-mono text-[11px]"
          />
          <Button
            size="sm"
            variant="outline"
            className="h-7"
            disabled={!canCopy}
            onClick={() => {
              setKeyDraft(`key:dup:${copyFrom}`, {
                op: "key_duplicate",
                index: copyFrom,
                time: parsedCopyTime,
              });
              setCopyFrom(null);
            }}
          >
            Copy
          </Button>
          <Button size="sm" variant="ghost" className="h-7" onClick={() => setCopyFrom(null)}>
            Cancel
          </Button>
        </div>
      )}
      {moveFrom !== null && (
        <div className="flex items-center gap-2 border-t border-border pt-2 text-[11px]">
          <span className="shrink-0 text-muted-foreground">Move key {moveFrom} to frame</span>
          <Input
            autoFocus
            value={moveTime}
            onChange={(e) => setMoveTime(e.target.value)}
            placeholder="Frame"
            className="h-7 w-28 font-mono text-[11px]"
          />
          <Button
            size="sm"
            variant="outline"
            className="h-7"
            disabled={!canMove}
            onClick={() => {
              setKeyDraft(`key:mv:${moveFrom}`, {
                op: "key_move",
                index: moveFrom,
                time: parsedMoveTime,
              });
              setMoveFrom(null);
            }}
          >
            Move
          </Button>
          <Button size="sm" variant="ghost" className="h-7" onClick={() => setMoveFrom(null)}>
            Cancel
          </Button>
        </div>
      )}
      <div className="flex items-center gap-2 border-t border-border pt-2 text-[11px]">
        <Tip
          content={
            frames.has(parsedTime)
              ? `Frame ${parsedTime} already has a key.`
              : "The new key's frame."
          }
        >
          <Input
            value={time}
            onChange={(e) => setTime(e.target.value)}
            placeholder="Frame"
            className={cn(
              "h-7 w-28 font-mono text-[11px]",
              frames.has(parsedTime) && "ring-1 ring-destructive"
            )}
          />
        </Tip>
        <Input
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="Value"
          className="h-7 min-w-0 flex-1 font-mono text-[11px]"
        />
        <Button
          size="sm"
          variant="outline"
          className="h-7"
          disabled={!canAdd}
          onClick={() => {
            setKeyDraft(`key:add:${parsedTime}`, {
              op: "key_add",
              time: parsedTime,
              value: parsedValue,
            });
            setTime("");
            setValue("");
          }}
        >
          <Plus size={13} /> Add key
        </Button>
      </div>
      <AlertDialogFooter>
        <AlertDialogCancel>Done</AlertDialogCancel>
      </AlertDialogFooter>
    </>
  );
}

function KeysAskDialog({
  ask,
  session,
  onClose,
}: {
  ask: KeysAsk | null;
  session: EditSession;
  onClose: () => void;
}) {
  return (
    <AlertDialog open={ask !== null} onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent>
        {ask && <KeysForm key={draftKey(ask.target)} ask={ask} session={session} />}
      </AlertDialogContent>
    </AlertDialog>
  );
}

/** The dialog around a KeyForm, queueing the insert with the key it collects. */
function KeyAskDialog({
  ask,
  session,
  onClose,
}: {
  ask: KeyAsk | null;
  session: EditSession;
  onClose: () => void;
}) {
  return (
    <AlertDialog open={ask !== null} onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent>
        {ask && (
          <KeyForm
            key={draftKey(ask.target)}
            ask={ask}
            onConfirm={(key) => {
              session.setDraft(
                ask.target,
                { op: "insert", index: ask.index, key: key || undefined },
                ask.within
              );
              onClose();
            }}
          />
        )}
      </AlertDialogContent>
    </AlertDialog>
  );
}

function PropertyTree({ rows }: { rows: TreeRow[] }) {
  const session = useEditSession();
  const [editingKey, setEditingKey] = useState<string | null>(null);
  const [menuRow, setMenuRow] = useState<TreeRow | null>(null);
  const [keyAsk, setKeyAsk] = useState<KeyAsk | null>(null);
  const [keysAsk, setKeysAsk] = useState<KeysAsk | null>(null);
  const menu = useMemo<TreeMenu>(
    () => ({ editingKey, setEditingKey, openMenu: setMenuRow }),
    [editingKey]
  );

  return (
    <TreeMenuContext.Provider value={menu}>
      <ContextMenu>
        <ContextMenuTrigger asChild>
          <div
            className="min-h-0 min-w-0 flex-1 overflow-auto"
            // Right-clicking the empty space below the rows has nothing to act on.
            onContextMenu={(e) => {
              if (!(e.target as HTMLElement).closest("[data-row]")) e.preventDefault();
            }}
          >
            {rows.map((row, i) => (
              <PropertyRow key={`${row.entry.name}-${i}`} row={row} depth={0} />
            ))}
          </div>
        </ContextMenuTrigger>
        <ContextMenuContent>
          {menuRow && (
            <RowMenu
              row={menuRow}
              session={session}
              onEdit={(key) => setEditingKey(key)}
              onAddKeyed={setKeyAsk}
              onKeys={setKeysAsk}
            />
          )}
        </ContextMenuContent>
      </ContextMenu>
      <KeyAskDialog ask={keyAsk} session={session} onClose={() => setKeyAsk(null)} />
      <KeysAskDialog ask={keysAsk} session={session} onClose={() => setKeysAsk(null)} />
    </TreeMenuContext.Provider>
  );
}

/** The menu for a row with a target: a leaf, a container, or an element of one. */
function RowMenu({
  row,
  session,
  onEdit,
  onAddKeyed,
  onKeys,
}: {
  row: TreeRow;
  session: EditSession;
  onEdit: (key: string) => void;
  onAddKeyed: (ask: KeyAsk) => void;
  onKeys: (ask: KeysAsk) => void;
}) {
  const { entry, target, within } = row;
  if (!target) return null;
  const key = draftKey(target);
  const draft = session.drafts[key]?.draft;
  const locked = rowLock(row, session);
  const discard = (
    <>
      <ContextMenuSeparator />
      <ContextMenuItem disabled={draft === undefined} onSelect={() => session.dropDraft(key)}>
        <Undo2 size={14} />
        Discard change
      </ContextMenuItem>
    </>
  );

  if (target.index !== undefined) {
    // An element's structural changes belong to its container, which is the key before its own.
    const { index, ...container } = target;
    const containerKey = draftKey(container);
    const containerWithin = within.slice(0, -1);
    const held = session.drafts[containerKey]?.draft;
    const structural = held !== undefined && isStructural(held);
    return (
      <>
        <ContextMenuItem disabled={!!locked} onSelect={() => onEdit(key)}>
          <Pencil size={14} />
          Edit element
        </ContextMenuItem>
        <ContextMenuItem
          // A copy is only valid in an array: a set keys on its contents.
          disabled={!!locked || structural || container.kind !== "array"}
          onSelect={() => session.setDraft(container, { op: "insert", index }, containerWithin)}
        >
          <Plus size={14} />
          Duplicate element
        </ContextMenuItem>
        <ContextMenuItem
          disabled={!!locked || structural}
          onSelect={() => session.setDraft(container, { op: "remove", index }, containerWithin)}
        >
          <Minus size={14} />
          Remove this element
        </ContextMenuItem>
        {discard}
      </>
    );
  }

  const count = elementCount(entry.value);
  if (count !== null) {
    const busy =
      !!session.locked ||
      hasDraftsWithin(session, key) ||
      (draft !== undefined && isStructural(draft));
    const keyed = entry.value.kind === "set" || entry.value.kind === "map";
    return (
      <>
        <Tip
          content="A set or a map keys on its contents, so the new element is added under a key you type."
          side="right"
          disabled={!keyed}
        >
          <ContextMenuItem
            disabled={busy}
            onSelect={() =>
              keyed
                ? onAddKeyed({
                    target,
                    within,
                    index: count,
                    label: displayName(entry.name),
                    kind: entry.value.kind === "set" ? "set" : "map",
                    structKey: keysAreStructs(entry.value),
                  })
                : session.setDraft(target, { op: "insert", index: count }, within)
            }
          >
            <Plus size={14} />
            {keyed ? "Add element…" : "Add element"}
          </ContextMenuItem>
        </Tip>
        <ContextMenuItem
          disabled={busy || count === 0}
          onSelect={() => session.setDraft(target, { op: "remove", index: count - 1 }, within)}
        >
          <Minus size={14} />
          Drop last element
        </ContextMenuItem>
        {discard}
      </>
    );
  }

  const stored = isStored(entry);
  const blocked = structuralLock(row, session);
  const isUnset = entry.value.kind === "unset";
  const storable = entry.value.kind === "unset" && !VALUE_ONLY_DECLARED.has(entry.value.declared);
  const keyChanges = isChannel(entry) ? pendingKeyCount(session, target) : 0;
  return (
    <>
      {isChannel(entry) && (
        <Tip
          content="A channel's keys are two arrays that change length together, so they are added and dropped here rather than as elements."
          side="right"
        >
          <ContextMenuItem
            disabled={!!blocked && keyChanges === 0}
            onSelect={() => onKeys({ target, within, entry })}
          >
            <Pencil size={14} />
            Keys…{keyChanges > 0 ? ` (${keyChanges} pending)` : ""}
          </ContextMenuItem>
        </Tip>
      )}
      <ContextMenuItem disabled={!!locked} onSelect={() => onEdit(key)}>
        <Pencil size={14} />
        Edit value
      </ContextMenuItem>
      {storable && (
        <Tip
          content="Writes the empty form of this type, so what is inside can be edited afterwards."
          side="right"
        >
          <ContextMenuItem
            disabled={!!blocked}
            onSelect={() => session.setDraft(target, { op: "store" }, within)}
          >
            <Plus size={14} />
            Store empty value
          </ContextMenuItem>
        </Tip>
      )}
      <Tip
        content="Flags the value as zero. The loader clears it rather than leaving the inherited value."
        side="right"
      >
        <ContextMenuItem
          disabled={!!blocked || (!stored && !isUnset && draft?.op !== "set")}
          onSelect={() => session.setDraft(target, { op: "clear" }, within)}
        >
          <Eraser size={14} />
          Set to zero
        </ContextMenuItem>
      </Tip>
      <Tip
        content="Drops the value so the object keeps the one it inherits from its parent."
        side="right"
      >
        <ContextMenuItem
          disabled={!!blocked || isUnset}
          onSelect={() => session.setDraft(target, { op: "unset" }, within)}
        >
          <Undo2 size={14} />
          Inherit default
        </ContextMenuItem>
      </Tip>
      {discard}
    </>
  );
}

const PropertyRow = memo(function PropertyRow({ row, depth }: { row: TreeRow; depth: number }) {
  const session = useEditSession();
  const menu = useContext(TreeMenuContext);
  const { entry, target, within } = row;
  const [open, setOpen] = useState(depth < 1);
  const expandable = isExpandable(entry.value);
  const label = labelOf(entry);
  const key = target ? draftKey(target) : null;
  const draft = key ? session.drafts[key]?.draft : undefined;
  const locked = rowLock(row, session);
  const editing = key !== null && menu?.editingKey === key;
  // A container element is always stored; only a property can hold its default.
  const stored = target?.index !== undefined || isStored(entry);
  const isUnset = entry.value.kind === "unset";
  const text =
    draftText(draft, elementCount(entry.value)) ??
    (stored || isUnset ? summarise(entry.value) : "(default)");
  const muted = entry.value.kind === "default" || (!stored && draft === undefined);

  const commit = (next: string) => {
    menu?.setEditingKey(null);
    if (!target || !key) return;
    // Typing a value back to what the file already holds is not a change.
    const original = stored ? initialText(entry, target) : null;
    if (next === original) {
      session.dropDraft(key);
      return;
    }
    session.setDraft(
      target,
      target.index === undefined
        ? { op: "set", text: next }
        : { op: "set_element", index: target.index, text: next },
      within
    );
  };

  return (
    <>
      <div
        data-row
        className={cn(
          "flex items-start gap-2 border-b border-border/30 py-1 pr-3 text-[11px]",
          expandable && "cursor-pointer hover:bg-muted/40"
        )}
        style={{ paddingLeft: `${depth * 14 + 12}px` }}
        onClick={expandable ? () => setOpen((v) => !v) : undefined}
        onContextMenu={(e) => {
          // A field inside an unset struct only takes a value; there is nothing to clear or drop.
          if (!target || target.path) {
            e.preventDefault();
            return;
          }
          menu?.openMenu(row);
        }}
      >
        <span className="mt-[2px] w-3 shrink-0 text-muted-foreground">
          {expandable ? open ? <ChevronDown size={11} /> : <ChevronRight size={11} /> : null}
        </span>
        <Tip content={label} disabled={displayName(label) === label}>
          <span className="w-[38%] max-w-[260px] min-w-[80px] shrink-0 truncate font-mono text-foreground/80">
            {displayName(label)}
          </span>
        </Tip>
        {editing ? (
          <span className="min-w-0 flex-1" onClick={(e) => e.stopPropagation()}>
            <ValueInput
              value={entry.value}
              initial={
                draft?.op === "set" || draft?.op === "set_element"
                  ? draft.text
                  : stored && target
                    ? initialText(entry, target)
                    : ""
              }
              onCommit={commit}
              onCancel={() => menu?.setEditingKey(null)}
            />
          </span>
        ) : (
          <Tip content={locked ?? (isUnset ? UNSET_HINT : null)}>
            <span
              className={cn(
                "min-w-0 flex-1 break-all font-mono",
                muted ? "text-muted-foreground/60 italic" : "text-foreground",
                draft !== undefined &&
                  "rounded-sm bg-blue-accent/15 px-1 text-blue-accent-foreground",
                !locked && "cursor-text hover:ring-1 hover:ring-inset hover:ring-primary/40"
              )}
              tabIndex={locked ? undefined : 0}
              onClick={(e) => {
                if (locked || !key) return;
                e.stopPropagation();
                menu?.setEditingKey(key);
              }}
              onKeyDown={(e) => {
                if (locked || !key) return;
                if (e.key === "Enter" || e.key === "F2") {
                  e.preventDefault();
                  e.stopPropagation();
                  menu?.setEditingKey(key);
                }
              }}
            >
              {text}
            </span>
          </Tip>
        )}
      </div>
      {expandable &&
        open &&
        childrenOf(row).map((child, i) => (
          <PropertyRow key={`${child.entry.name}-${i}`} row={child} depth={depth + 1} />
        ))}
    </>
  );
});

function StatusBadge({
  status,
  note,
  undecoded,
}: {
  status: ExportStatus;
  note?: string;
  undecoded?: UndecodedPayload[];
}) {
  const stopped = note ? ` The layout walk stopped: ${note}` : "";
  // A payload that did not decode leaves the export exact, because its length prefix puts the
  // cursor back. Saying only "exact" would hide it.
  const hidden = undecoded?.length
    ? ` ${undecoded.length} instanced payload(s) inside it did not decode: ${undecoded
        .slice(0, 5)
        .map((p) => `${p.struct_name}: ${p.reason}`)
        .join("; ")}`
    : "";
  if (status.state === "complete") {
    if (undecoded?.length) {
      return (
        <Tip content={`Every declared byte was accounted for, but${hidden}`} side="bottom">
          <span className="flex items-center gap-1 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-amber-400">
            <AlertTriangle size={10} /> exact, {undecoded.length} undecoded
          </span>
        </Tip>
      );
    }
    return (
      <Tip content="Every declared byte was accounted for" side="bottom">
        <span className="flex items-center gap-1 rounded bg-emerald-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-emerald-400">
          <Check size={10} /> exact
        </span>
      </Tip>
    );
  }
  if (status.state === "payload") {
    return (
      <Tip
        content={`Every property decoded. The remaining ${formatBytes(status.payload_bytes)} is ${status.kind}, which this tool measures but does not decode.${stopped}`}
        side="bottom"
      >
        <span className="flex items-center gap-1 rounded bg-sky-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-sky-400">
          <Database size={10} /> {status.kind}
        </span>
      </Tip>
    );
  }
  if (status.state === "partial") {
    return (
      <Tip
        content={`Properties decoded, but ${status.expected - status.consumed} trailing bytes could not be attributed to anything known. Treat the values with care and check the undecoded bytes below.${stopped}`}
        side="bottom"
      >
        <span className="flex items-center gap-1 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-amber-400">
          <AlertTriangle size={10} /> {status.expected - status.consumed} bytes unexplained
        </span>
      </Tip>
    );
  }
  return (
    <Tip content={status.reason} side="bottom">
      <span className="flex items-center gap-1 rounded bg-red-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-red-400">
        <AlertTriangle size={10} /> failed
      </span>
    </Tip>
  );
}

// Sentinel key for the row-name column. UE property names cannot contain a space.
const ROW_KEY = "row name";
const MIN_COLUMN = 60;
const MAX_AUTO_COLUMN = 420;
// Ceiling for a column that already fits, so filling never turns a flag column into a banner.
const MAX_FILLED_COLUMN = 320;
// A column whose content was already clipped may take much more, since it has text to show.
const MAX_CLIPPED_COLUMN = 900;

/** UE appends `_<index>_<guid>` to Blueprint and row struct fields. The suffix is noise on screen,
 *  but the raw name stays the key, shows on hover, and is what the JSON and CSV views export. The
 *  hex run is matched loosely rather than at exactly 32 so a non-standard length still cleans up. */
function displayName(name: string): string {
  return name.replace(/_\d+_[0-9A-F]{16,}(?=(\[\d+\])?$)/i, "");
}

function autoWidth(longest: number): number {
  return Math.min(MAX_AUTO_COLUMN, Math.max(MIN_COLUMN, longest * 7.1 + 26));
}

function HeaderCell({
  label,
  title,
  width,
  onResize,
}: {
  label: string;
  title: string;
  width: number;
  onResize: (event: React.MouseEvent) => void;
}) {
  return (
    <Tip content={title} disabled={title === label}>
      <div className="relative shrink-0 truncate px-2 py-1.5" style={{ width }}>
        {label}
        <span
          onMouseDown={onResize}
          className="absolute inset-y-0 right-0 w-1 cursor-col-resize hover:bg-primary/60"
        />
      </div>
    </Tip>
  );
}

/** Escape hatch for anything the tree and table views summarise away. */
interface HexRow {
  offset: number;
  hex: string;
  ascii: string;
}

interface ByteRange {
  name: string;
  kind: string;
  depth: number;
  start: number;
  end: number;
}

interface BytesView {
  rows: HexRow[];
  base: number;
  size: number;
  stopped_at: number | null;
  ranges: ByteRange[];
}

// Raw bytes at the offsets traces and failure messages quote, with the run each property consumed
// shaded so a desync can be read against the data that caused it.
/** The disassembly of one export's bytecode. Read only: an instruction cannot be edited yet, but
 *  the bytes behind it can be swapped whole through the export's Replace action. */
function ScriptPane({
  gamePath,
  container,
  entry,
  exportIndex,
  exportNames,
  focus,
  onOpen,
}: {
  gamePath: string;
  container: string;
  entry: string;
  exportIndex: number;
  /** The package's exports by name, so a call to one of them can be followed. */
  exportNames: Set<string>;
  /** A statement to scroll to on opening, when the pane was opened from a call or a caller. */
  focus: number | null;
  onOpen: (name: string, offset?: number) => void;
}) {
  const [view, setView] = useState<ScriptView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);

  // The pane is keyed by export, so it mounts fresh for each one and the effect never has to
  // reset what the last one left behind.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const result = await invoke<ScriptView>("export_script_view", {
          gameRoot: gamePath,
          container,
          entry,
          export: exportIndex,
        });
        if (!cancelled) setView(result);
      } catch (e) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setBusy(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [gamePath, container, entry, exportIndex]);

  if (busy) {
    return <p className="p-6 text-center text-sm text-muted-foreground">Disassembling…</p>;
  }
  if (error || !view) {
    return <p className="p-6 text-center text-sm text-red-400">{error ?? "No script"}</p>;
  }
  return (
    <div className="min-h-0 min-w-0 flex-1 overflow-auto">
      <div className="flex items-center gap-3 border-b border-border/60 px-3 py-1.5 text-[11px] text-muted-foreground">
        <span>
          {view.statements} statement{view.statements === 1 ? "" : "s"}
        </span>
        <span>{view.storage_size} bytes stored</span>
        <span>{view.buffer_size} loaded</span>
        {!view.complete && (
          <span className="flex items-center gap-1 text-amber-400">
            <AlertTriangle size={10} /> stopped: {view.stopped}
          </span>
        )}
      </div>
      {view.signature_text && view.signature && (
        <div className="border-b border-border/60 px-3 py-1.5 font-mono text-[11px]">
          <p className="text-blue-accent-foreground">{view.signature_text}</p>
          {view.signature.locals.length > 0 && (
            <details className="mt-0.5 text-muted-foreground">
              <summary className="cursor-pointer font-sans hover:text-foreground">
                {view.signature.locals.length} local
                {view.signature.locals.length === 1 ? "" : "s"}
              </summary>
              <div className="mt-1 grid grid-cols-[auto_1fr] gap-x-3 pl-3">
                {view.signature.locals.map((local) => (
                  <React.Fragment key={local.name}>
                    <span>{local.name}</span>
                    <span className="text-muted-foreground">{local.kind}</span>
                  </React.Fragment>
                ))}
              </div>
            </details>
          )}
        </div>
      )}
      {view.callers.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5 border-b border-border/60 px-3 py-1.5 text-[11px] text-muted-foreground">
          <span>Called by:</span>
          {view.callers.map(([caller, offset]) => (
            <button
              key={`${caller}:${offset}`}
              className="font-mono text-blue-accent-foreground hover:underline"
              onClick={() => onOpen(caller, offset)}
            >
              {caller} {hex(offset)}
            </button>
          ))}
        </div>
      )}
      <ScriptLines
        exportIndex={exportIndex}
        lines={view.lines}
        entries={view.entries}
        exportNames={exportNames}
        focus={focus}
        onOpen={onOpen}
      />
    </div>
  );
}

const hex = (offset: number) => `0x${offset.toString(16).toUpperCase().padStart(4, "0")}`;

/** Statements with their jump, pushed-flow and resume targets as links, and the events that
 *  enter an Ubergraph named where their code starts. */
function ScriptLines({
  exportIndex,
  lines,
  entries,
  exportNames,
  focus,
  onOpen,
}: {
  exportIndex: number;
  lines: ScriptLine[];
  entries: [number, string][];
  exportNames: Set<string>;
  focus: number | null;
  onOpen: (name: string, offset?: number) => void;
}) {
  const [focused, setFocused] = useState<number | null>(focus);
  const rowRefs = useRef(new Map<number, HTMLDivElement>());

  useEffect(() => {
    if (focus !== null) rowRefs.current.get(focus)?.scrollIntoView({ block: "center" });
  }, [focus]);

  const incoming = useMemo(() => {
    const from = new Map<number, number[]>();
    for (const line of lines) {
      for (const target of line.targets ?? []) {
        const list = from.get(target) ?? [];
        list.push(line.offset);
        from.set(target, list);
      }
    }
    return from;
  }, [lines]);

  const eventsAt = useMemo(() => {
    const at = new Map<number, string[]>();
    for (const [offset, name] of entries) at.set(offset, [...(at.get(offset) ?? []), name]);
    return at;
  }, [entries]);

  const go = (offset: number) => {
    setFocused(offset);
    rowRefs.current.get(offset)?.scrollIntoView({ block: "center", behavior: "smooth" });
  };

  return (
    <div className="px-3 py-2 font-mono text-[11px] leading-relaxed">
      {entries.length > 0 && (
        <div className="mb-2 flex flex-wrap items-center gap-1.5 font-sans text-muted-foreground">
          <span>Events:</span>
          {entries.map(([offset, name]) => (
            <button
              key={`${offset}:${name}`}
              className="rounded border border-blue-accent-border bg-blue-accent px-1.5 py-0.5 text-blue-accent-foreground hover:bg-blue-accent-hover"
              onClick={() => go(offset)}
            >
              {name}
            </button>
          ))}
        </div>
      )}
      {lines.map((line) => {
        const from = incoming.get(line.offset);
        return (
          <React.Fragment key={line.offset}>
            {eventsAt.get(line.offset)?.map((name) => (
              <div key={name} className="mt-2 font-sans font-semibold text-blue-accent-foreground">
                {name}
              </div>
            ))}
            <div
              ref={(el) => {
                if (el) rowRefs.current.set(line.offset, el);
                else rowRefs.current.delete(line.offset);
              }}
              className={cn(
                "flex gap-3 whitespace-pre rounded-sm",
                focused === line.offset && "bg-primary/15"
              )}
            >
              <span className="w-12 shrink-0 text-muted-foreground">
                {hex(line.offset)}
                {from && (
                  <Tip content={`Reached from ${from.map(hex).join(", ")}`}>
                    <span className="ml-0.5 text-blue-accent-foreground">•</span>
                  </Tip>
                )}
              </span>
              <span>{line.text}</span>
              {(line.literals ?? []).map((slot) => (
                <LiteralChip
                  key={slot.index}
                  exportIndex={exportIndex}
                  statement={line.offset}
                  slot={slot}
                />
              ))}
              {(line.targets ?? []).map((target) => (
                <button
                  key={target}
                  className="shrink-0 font-sans text-blue-accent-foreground hover:underline"
                  onClick={() => go(target)}
                >
                  → {hex(target)}
                </button>
              ))}
              {(line.calls ?? [])
                .filter((call) => exportNames.has(call))
                .map((call) => (
                  <Tip key={call} content={`Open ${call}`}>
                    <button
                      className="shrink-0 font-sans text-blue-accent-foreground hover:underline"
                      onClick={() => onOpen(call)}
                    >
                      ↗ {call}
                    </button>
                  </Tip>
                ))}
            </div>
          </React.Fragment>
        );
      })}
    </div>
  );
}

/** A constant in a statement that can take a new value in place. */
interface LiteralSlot {
  /** Which literal in the statement, counting every literal, which is how an edit names it. */
  index: number;
  kind: string;
  value: string;
  /** A string's length, which a replacement has to keep. */
  length?: number;
}

/** How long a string constant's replacement is, in the units its width is counted in. */
function literalLength(kind: string, text: string): number {
  return kind === "UnicodeStringConst" ? text.length : [...text].length;
}

/** One editable constant: its value, or the draft replacing it, and an inline field to change it.
 *  The bytes cannot grow in place, so a string keeps its length and a number its type. */
function LiteralChip({
  exportIndex,
  statement,
  slot,
}: {
  exportIndex: number;
  statement: number;
  slot: LiteralSlot;
}) {
  const session = useEditSession();
  const target = scriptTarget(exportIndex, statement, slot.index);
  const key = draftKey(target);
  const draft = session.drafts[key]?.draft;
  const current = draft?.op === "script_set" ? draft.text : slot.value;
  const [editing, setEditing] = useState<string | null>(null);

  const problem =
    editing !== null &&
    slot.length !== undefined &&
    literalLength(slot.kind, editing) !== slot.length
      ? `Must stay ${slot.length} characters long; this is ${literalLength(slot.kind, editing)}.`
      : null;
  const commit = () => {
    if (editing === null || problem) return;
    if (editing === slot.value) session.dropDraft(key);
    else session.setDraft(target, { op: "script_set", text: editing }, []);
    setEditing(null);
  };

  if (editing !== null) {
    return (
      <Tip content={problem ?? `${slot.kind}. Enter to keep, Esc to cancel.`}>
        <input
          autoFocus
          value={editing}
          onChange={(e) => setEditing(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") setEditing(null);
          }}
          onBlur={() => (problem ? setEditing(null) : commit())}
          size={Math.max(editing.length, 4)}
          className={cn(
            "shrink-0 rounded-sm border bg-background px-1 font-mono text-[11px] outline-none",
            problem ? "border-err" : "border-primary"
          )}
        />
      </Tip>
    );
  }
  const hint =
    session.locked ??
    `${slot.kind}${slot.length !== undefined ? `, ${slot.length} characters` : ""}. Click to change it.`;
  return (
    <span className="flex shrink-0 items-center">
      <Tip content={hint}>
        <button
          disabled={!!session.locked}
          onClick={() => setEditing(current)}
          className={cn(
            "max-w-64 truncate rounded-sm border px-1 font-mono",
            draft
              ? "border-blue-accent-border bg-blue-accent/15 text-blue-accent-foreground"
              : "border-border/60 text-muted-foreground hover:text-foreground"
          )}
        >
          {current === "" ? "\u2205" : current}
        </button>
      </Tip>
      {draft && (
        <button
          className="px-0.5 text-muted-foreground hover:text-foreground"
          onClick={() => session.dropDraft(key)}
        >
          <X size={10} />
        </button>
      )}
    </span>
  );
}

interface ScriptLine {
  offset: number;
  text: string;
  targets?: number[];
  calls?: string[];
  literals?: LiteralSlot[];
}

interface FunctionField {
  name: string;
  kind: string;
  role: "in" | "ref" | "out" | "return" | "local";
}

interface ScriptView {
  lines: ScriptLine[];
  entries: [number, string][];
  signature: { params: FunctionField[]; locals: FunctionField[] } | null;
  signature_text: string | null;
  callers: [string, number][];
  complete: boolean;
  stopped: string | null;
  buffer_size: number;
  storage_size: number;
  statements: number;
}

function BytesPane({
  gamePath,
  container,
  entry,
  exportIndex,
}: {
  gamePath: string;
  container: string;
  entry: string;
  exportIndex: number;
}) {
  const [view, setView] = useState<BytesView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  const [hovered, setHovered] = useState<ByteRange | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    setBusy(true);
    setError(null);
    setView(null);
    void (async () => {
      try {
        const result = await invoke<BytesView>("export_bytes_view", {
          gameRoot: gamePath,
          container,
          entry,
          export: exportIndex,
        });
        if (!cancelled) setView(result);
      } catch (e) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setBusy(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [gamePath, container, entry, exportIndex]);

  const rows = view?.rows ?? [];
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 18,
    overscan: 24,
  });

  // A byte belongs to the innermost property covering it, which is the last range that contains it.
  const owner = useMemo(() => {
    const map = new Map<number, ByteRange>();
    for (const range of view?.ranges ?? []) {
      for (let at = range.start; at < range.end; at += 1) map.set(at, range);
    }
    return map;
  }, [view]);

  const copy = () =>
    void navigator.clipboard.writeText(
      rows.map((r) => `0x${r.offset.toString(16).toUpperCase()}  ${r.hex}  ${r.ascii}`).join("\n")
    );

  if (busy) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center gap-2 text-sm text-muted-foreground">
        <Loader2 size={14} className="animate-spin" /> Reading bytes
      </div>
    );
  }
  if (error || !view) {
    return (
      <p className="max-w-2xl break-words p-6 text-sm text-muted-foreground">
        {error ?? "No bytes to show."}
      </p>
    );
  }

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
        <span className="min-w-0 truncate text-xs text-muted-foreground">
          {view.size.toLocaleString()} bytes at 0x{view.base.toString(16).toUpperCase()}
          {view.stopped_at !== null &&
            `, decoded to 0x${view.stopped_at.toString(16).toUpperCase()}`}
        </span>
        <span className="min-w-0 flex-1 truncate text-right text-xs text-sky-400">
          {hovered && `${hovered.name} : ${hovered.kind}`}
        </span>
        <Button size="sm" variant="outline" className="h-7 shrink-0" onClick={copy}>
          <Copy size={12} /> Copy all
        </Button>
      </div>
      <div ref={scrollRef} className="min-h-0 min-w-0 flex-1 overflow-auto px-3 py-2">
        <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
          {virtualizer.getVirtualItems().map((item) => {
            const row = rows[item.index];
            const tokens = row.hex.split(" ");
            return (
              <div
                key={row.offset}
                className="absolute left-0 top-0 flex w-full gap-3 whitespace-pre font-mono text-[11px] leading-[18px]"
                style={{ height: item.size, transform: `translateY(${item.start}px)` }}
              >
                <span className="shrink-0 tabular-nums text-muted-foreground/60">
                  {row.offset.toString(16).toUpperCase().padStart(8, "0")}
                </span>
                <span className="shrink-0">
                  {tokens.map((token, i) => {
                    const at = row.offset + i;
                    const range = owner.get(at);
                    const stop = view.stopped_at === at;
                    return (
                      <span
                        key={i}
                        onMouseEnter={() => setHovered(range ?? null)}
                        className={cn(
                          "px-px",
                          range ? "text-foreground" : "text-muted-foreground",
                          range && range.depth % 2 === 1 && "bg-sky-500/10",
                          range && range.depth % 2 === 0 && "bg-sky-500/20",
                          stop && "border-l border-red-400 bg-red-500/20"
                        )}
                      >
                        {token}
                      </span>
                    );
                  })}
                </span>
                <span className="shrink-0 text-muted-foreground/70">{row.ascii}</span>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

function JsonView({ export: parsed }: { export: ParsedExport }) {
  const text = useMemo(() => JSON.stringify(parsed, null, 2), [parsed]);
  // Rendering megabytes of text locks the window up; copying all of it does not.
  const LIMIT = 400_000;
  const shown =
    text.length > LIMIT
      ? `${text.slice(0, LIMIT)}
... truncated for display`
      : text;

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
        <span className="min-w-0 truncate text-xs text-muted-foreground">
          {text.length.toLocaleString()} characters of JSON
          {text.length > LIMIT && ", display truncated"}
        </span>
        <Button
          size="sm"
          variant="outline"
          className="ml-auto h-7 shrink-0"
          onClick={() => void navigator.clipboard.writeText(text)}
        >
          <Copy size={12} /> Copy all
        </Button>
      </div>
      <pre className="min-h-0 min-w-0 flex-1 overflow-auto px-3 py-2 font-mono text-[11px] leading-relaxed text-foreground/80">
        {shown}
      </pre>
    </div>
  );
}

/** Kinds the writer can produce bytes for from typed text. */
const EDITABLE_KINDS = new Set([
  "bool",
  "int",
  "uint",
  "float",
  "byte",
  "enum",
  "str",
  "name",
  "soft_object",
  "text",
  "object",
]);

/** Kinds that hold elements, which are changed by adding or dropping rather than by retyping. */
const CONTAINER_KINDS = new Set(["array", "set", "map"]);

/** Declared storage kinds the writer can produce from typed text when the slot holds nothing yet. */
const TYPEABLE_DECLARED = new Set([
  // Native structs that hold one value, typed in whole.
  "SoftObjectPath",
  "SoftClassPath",
  "TopLevelAssetPath",
  "MarvelSoftObjectPath",
  "Guid",
  "Str",
  "Utf8Str",
  "AnsiStr",
  "Name",
  "SoftObject",
  "AssetObject",
  "Object",
  "WeakObject",
  "Interface",
  "Text",
  "Bool",
  "Byte",
  "Int8",
  "Int16",
  "Int",
  "Int64",
  "UInt16",
  "UInt32",
  "UInt64",
  "Float",
  "Double",
]);

/** Declared kinds with no empty form worth storing: they need a value, so typing is the only way. */
const VALUE_ONLY_DECLARED = new Set([
  "Str",
  "Utf8Str",
  "AnsiStr",
  "Bool",
  "Byte",
  "Int8",
  "Int16",
  "Int",
  "Int64",
  "UInt16",
  "UInt32",
  "UInt64",
  "Float",
  "Double",
]);

const UNSET_HINT =
  "Not stored: takes the parent class's value (or the struct's default). Set it to store a value of its own.";

/** Whether an unset slot can be typed into, or has to be stored first. */
function unsetReason(declared: string): string | null {
  return TYPEABLE_DECLARED.has(declared)
    ? null
    : "Right click to store it first, then edit what is inside.";
}

/** How many elements a container holds, or `null` for anything that is not one. */
function elementCount(value: PropertyValue): number | null {
  switch (value.kind) {
    case "array":
    case "set":
      return value.items.length;
    case "map":
      return value.entries.length;
    default:
      return null;
  }
}

/** A value the header flagged as its default occupies no bytes, so its range is empty. */
function isStored(field: PropertyEntry): boolean {
  return !!field.span && field.span[1] > field.span[0];
}

/**
 * The text an edit works on, which is not always what the cell displays: an enum shows its
 * enumerator name but is written as the underlying number, and a float shows a rounded summary.
 */
function editText(value: PropertyValue): string {
  switch (value.kind) {
    // The encoder takes an enumerator name or the number, so the name is what a person edits.
    case "enum":
      return value.name ?? String(value.value);
    case "float":
      return String(value.value);
    case "bool":
      return value.value ? "true" : "false";
    // An object reference is written as an index, but only something the package already names can
    // be pointed at, so the path is what a person can sensibly retype.
    case "object":
      return value.path ?? String(value.index);
    default:
      return summarise(value);
  }
}

/** A container element goes through its own serialization, which writes an enum as its
 *  enumerator name rather than as the number a struct-level enum takes. */
function initialText(entry: PropertyEntry, target: EditTarget): string {
  if (target.index !== undefined && entry.value.kind === "enum") return summarise(entry.value);
  return editText(entry.value);
}

/** What a cell shows while a change is queued, or null when nothing is. */
function draftText(draft: Draft | undefined, count: number | null): string | null {
  if (!draft) return null;
  switch (draft.op) {
    case "set":
    case "set_element":
      return draft.text;
    case "clear":
      return "(zero)";
    case "store":
      return "(stored, empty)";
    case "unset":
      return "(inherited)";
    case "insert":
      return `${(count ?? 0) + 1} items`;
    case "remove":
      return `${Math.max((count ?? 1) - 1, 0)} items`;
    default:
      return null;
  }
}

/**
 * A cell can be edited when its kind is one the writer can produce bytes for. Holding a default
 * is no barrier: the value is given bytes of its own and the header flag is cleared. A container
 * has no text to type, so it is not editable in this sense even though its elements can be added
 * and dropped.
 */
function editableReason(field: PropertyEntry | undefined): string | null {
  if (!field) return "This row does not store this column.";
  if (!field.span) return NO_POSITION;
  if (field.value.kind === "unset") return unsetReason(field.value.declared);
  if (field.value.kind === "default" && field.value.declared) {
    return unsetReason(field.value.declared);
  }
  if (field.value.kind === "text" && field.value.parts?.length) {
    return "This text is built from the parts below; edit one of them.";
  }
  if (CONTAINER_KINDS.has(field.value.kind)) {
    return "Right click to add or drop an element.";
  }
  if (!EDITABLE_KINDS.has(field.value.kind)) {
    return `${field.value.kind} values cannot be edited yet.`;
  }
  return null;
}

/** Why nothing in an export can be edited, or null when it can. `payload` is fine: the property
 *  block decoded fully and the bytes after it are never touched by a splice. */
function lockedReasonOf(exp: ParsedExport): string | null {
  switch (exp.status.state) {
    case "failed":
      return "This export did not decode, so it cannot be edited.";
    case "partial":
      return `${exp.status.expected - exp.status.consumed} trailing bytes are unexplained, so this export cannot be edited until they are understood.`;
    default:
      return exp.data_table?.truncated
        ? "Rows after the break are missing, so this table cannot be edited."
        : null;
  }
}

/** Enter keeps the value, Escape abandons it. Modelled on the mod rename field. */
interface EnumOption {
  value: number;
  name: string;
}

const enumOptionCache = new Map<string, Promise<EnumOption[]>>();

/** The enumerators of a type, fetched once per session; empty when the mappings do not know it. */
function loadEnumOptions(enumType: string): Promise<EnumOption[]> {
  let pending = enumOptionCache.get(enumType);
  if (!pending) {
    pending = invoke<EnumOption[]>("enum_options", { enumType }).catch(() => []);
    enumOptionCache.set(enumType, pending);
  }
  return pending;
}

/** `null` while nothing is known yet, so the cell can fall back to free text. */
function useEnumOptions(enumType: string | undefined): EnumOption[] | null {
  const [options, setOptions] = useState<EnumOption[] | null>(null);
  useEffect(() => {
    if (!enumType) return;
    let live = true;
    void loadEnumOptions(enumType).then((found) => {
      if (live) setOptions(found);
    });
    return () => {
      live = false;
    };
  }, [enumType]);
  return enumType ? options : null;
}

/** A picker over an enum's enumerators. A value the mappings do not name stays selectable. */
function CellSelect({
  options,
  initial,
  onCommit,
  onCancel,
}: {
  options: EnumOption[];
  initial: string;
  onCommit: (text: string) => void;
  onCancel: () => void;
}) {
  const done = useRef(false);
  const commit = (text: string) => {
    if (done.current) return;
    done.current = true;
    onCommit(text);
  };
  const known = options.some((option) => option.name === initial);
  return (
    <select
      autoFocus
      defaultValue={initial}
      onClick={(e) => e.stopPropagation()}
      onChange={(e) => commit(e.target.value)}
      onBlur={(e) => commit(e.target.value)}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Escape") {
          done.current = true;
          onCancel();
        }
      }}
      className="h-full w-full rounded-sm bg-background px-1 font-mono text-[11px] text-foreground outline-none ring-1 ring-primary"
    >
      {!known && <option value={initial}>{initial}</option>}
      {options.map((option) => (
        <option key={option.value} value={option.name}>
          {option.name}
        </option>
      ))}
    </select>
  );
}

/** The in-place editor for a value: a picker for an enum the mappings know, text otherwise. */
function ValueInput({
  value,
  initial,
  onCommit,
  onCancel,
}: {
  value: PropertyValue;
  initial: string;
  onCommit: (text: string) => void;
  onCancel: () => void;
}) {
  const enumType = value.kind === "enum" || value.kind === "unset" ? value.enum_type : undefined;
  const options = useEnumOptions(enumType);
  if (options && options.length > 0) {
    return (
      <CellSelect options={options} initial={initial} onCommit={onCommit} onCancel={onCancel} />
    );
  }
  return <CellInput initial={initial} onCommit={onCommit} onCancel={onCancel} />;
}

function CellInput({
  initial,
  onCommit,
  onCancel,
}: {
  initial: string;
  onCommit: (text: string) => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState(initial);
  const done = useRef(false);
  const commit = () => {
    if (done.current) return;
    done.current = true;
    onCommit(text);
  };
  return (
    <input
      autoFocus
      value={text}
      onChange={(e) => setText(e.target.value)}
      onClick={(e) => e.stopPropagation()}
      onFocus={(e) => e.currentTarget.select()}
      onBlur={commit}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") {
          e.currentTarget.blur();
        } else if (e.key === "Escape") {
          done.current = true;
          onCancel();
        }
      }}
      className="h-full w-full rounded-sm bg-background px-2 font-mono text-[11px] text-foreground outline-none ring-1 ring-primary"
    />
  );
}

interface ModsStatus {
  mod_entries: {
    full_name: string;
    display_name: string;
    enabled: boolean;
    has_companions: boolean;
    kind: "Pak" | "IoStore";
  }[];
}

/** Mods a save can be pointed at: plain paks whose name round-trips through the normaliser, so
 *  picking one really does target that file. */
function useModNameSuggestions(gamePath: string): string[] {
  const [names, setNames] = useState<string[]>([]);
  useEffect(() => {
    let cancelled = false;
    invoke<ModsStatus>("get_mods_status", { gameRoot: gamePath })
      .then((status) => {
        if (cancelled) return;
        const stems = status.mod_entries
          .filter(
            (mod) =>
              mod.enabled &&
              mod.kind === "Pak" &&
              !mod.has_companions &&
              !mod.full_name.includes("/") &&
              /_9999999_P\.pak$/i.test(mod.display_name)
          )
          .map((mod) => mod.display_name.replace(/_9999999_P\.pak$/i, ""));
        setNames([...new Set(stems)].sort((a, b) => a.localeCompare(b)));
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [gamePath]);
  return names;
}

/** Object flags worth naming; the rest read as hex. */
/** The flag bits the backend will write. Anything else an export holds is shown but not offered:
 *  the loader drops the rest, so setting one would be a change that does not survive the trip. */
const OBJECT_FLAGS: [number, string][] = [
  [0x1, "Public"],
  [0x2, "Standalone"],
  [0x8, "Transactional"],
  [0x10, "ClassDefaultObject"],
  [0x20, "ArchetypeObject"],
  [0x40000, "DefaultSubObject"],
  [0x100000, "TextExportTransient"],
  [0x400000, "InheritableComponentTemplate"],
  [0x800000, "DuplicateTransient"],
  [0x2000000, "NonPIEDuplicateTransient"],
];

/** A class default object is the one its class names, so that bit is shown and never offered. */
const RF_CLASS_DEFAULT_OBJECT = 0x10;

function flagNames(flags: number): string {
  const named = OBJECT_FLAGS.filter(([bit]) => flags & bit).map(([, name]) => name);
  return named.length > 0 ? named.join(" ") : `0x${flags.toString(16)}`;
}

/** The path an FPackageIndex names, for the tables that show references. */
function referencePath(pkg: ParsedPackage, index: number): string {
  if (index === 0) return "";
  if (index < 0) return pkg.imports.find((i) => i.index === index)?.path ?? `import ${index}`;
  return pkg.exports[index - 1]?.path ?? `export ${index}`;
}

/** An import path cell: click to retarget the import at another object. */
function ImportPathCell({ info, edits }: { info: ImportInfo; edits: AssetEdits }) {
  const key = `retarget:${info.index}`;
  const draft = edits.importDrafts[key];
  const [editing, setEditing] = useState(false);
  const locked = edits.session.locked;
  if (editing) {
    return (
      <CellInput
        initial={draft?.path ?? info.path}
        onCommit={(next) => {
          setEditing(false);
          const text = next.trim();
          if (text === info.path || text === "") edits.dropImportDraft(key);
          else edits.setImportDraft(key, { index: info.index, path: text });
        }}
        onCancel={() => setEditing(false)}
      />
    );
  }
  return (
    <Tip
      content={
        locked ??
        (info.object_name === "UnknownExport"
          ? "An older converter dropped this import's hash when the package was extracted. Re-extract it from its container to recover the reference."
          : info.unresolved
            ? "retoc could not resolve this object when converting the package; the hash stands in for it."
            : "Click to point this import at another object: /Game/Path/Asset.Object, or :Sub for a subobject.")
      }
    >
      <span
        className={cn(
          "block min-w-0 truncate font-mono",
          info.unresolved && "text-muted-foreground italic",
          draft !== undefined && "rounded-sm bg-blue-accent/15 px-1 text-blue-accent-foreground",
          !locked && "cursor-text hover:ring-1 hover:ring-inset hover:ring-primary/40"
        )}
        onClick={() => {
          if (!locked) setEditing(true);
        }}
      >
        {draft?.path ?? info.path}
      </span>
    </Tip>
  );
}

/** A form for an import the package never had, laid out in the import table's own columns and
 *  pinned to the bottom of it so it stays in reach however long the table is. The class is typed
 *  the way the class column's tooltip shows it: `/Script/Package.Class`. */
function AddImportRow({
  edits,
  serial,
  cell,
}: {
  edits: AssetEdits;
  serial: number;
  cell: string;
}) {
  const [path, setPath] = useState("");
  const [classPath, setClassPath] = useState("/Script/CoreUObject.Object");
  const valid = /^\/[^.]+\.[^.:]+(:[^.:]+)*$/.test(path.trim());
  const add = () => {
    const text = classPath.trim();
    const dot = text.lastIndexOf(".");
    edits.setImportDraft(`add:${serial}`, {
      path: path.trim(),
      classPackage: dot > 0 ? text.slice(0, dot) : undefined,
      className: (dot > 0 ? text.slice(dot + 1) : text) || undefined,
    });
    setPath("");
  };
  const disabled = !valid || !!edits.session.locked;
  return (
    // A collapsed border stays with the table rather than the sticky row, so the rule is a shadow.
    <tfoot className="sticky bottom-0 z-10 bg-background [&_td]:shadow-[inset_0_1px_0_var(--color-border)]">
      <tr>
        <td className={cn(cell, "text-muted-foreground")}>
          <Plus size={12} />
        </td>
        <td className="px-1 py-1">
          <Tip content="The object's class, as /Script/Package.Class. Object is accepted for anything, but a real class reads better.">
            <Input
              value={classPath}
              onChange={(e) => setClassPath(e.target.value)}
              placeholder="/Script/Engine.StaticMesh"
              className="h-7 w-full font-mono text-[11px]"
            />
          </Tip>
        </td>
        <td className="px-1 py-1" colSpan={2}>
          <Input
            value={path}
            onChange={(e) => setPath(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !disabled) add();
            }}
            placeholder="/Game/Path/Asset.Object"
            className="h-7 w-full font-mono text-[11px]"
          />
        </td>
        <td className="px-1 py-1">
          <Tip content={edits.session.locked ?? "Add import"}>
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-1.5"
              disabled={disabled}
              onClick={add}
            >
              <Plus size={13} />
            </Button>
          </Tip>
        </td>
      </tr>
    </tfoot>
  );
}

/** The package's own tables: names, imports and exports, with import paths editable and the
 *  export actions offered on each export row. */
/** Where a payload's bytes go and come from: a file picked in a dialog. */
interface BytesActions {
  exportPayload: (index: number, name: string) => void;
  replacePayload: (index: number) => void;
  exportBulk: (index: number) => void;
  replaceBulk: (index: number) => void;
}

function PackageView({
  pkg,
  edits,
  lock,
  bytes,
  onSelectExport,
  onRemove,
  onReset,
  onDuplicate,
  onRename,
  onFlags,
  onRetype,
  onCopyOut,
  clipboard,
  onPaste,
  onDeps,
  onDropImport,
}: {
  pkg: ParsedPackage;
  edits: AssetEdits;
  lock: string | null;
  bytes: BytesActions;
  onSelectExport: (index: number) => void;
  onRemove: (index: number) => void;
  onReset: (index: number) => void;
  onDuplicate: (index: number) => void;
  onRename: (index: number) => void;
  onFlags: (index: number) => void;
  onRetype: (index: number) => void;
  onCopyOut: (index: number) => void;
  /** What is marked for pasting, if anything. */
  clipboard: ExportClipboard | null;
  onPaste: () => void;
  onDeps: (index: number) => void;
  onDropImport: (at: number) => void;
}) {
  const adds = Object.entries(edits.importDrafts).filter(([key]) => key.startsWith("add:"));
  const payloadActions = (exp: ParsedExport): PayloadActions => {
    const key = draftKey(payloadTarget(exp.index));
    return {
      lock: payloadLockOf(exp, pkg),
      drafted: edits.session.drafts[key] !== undefined,
      onExport: () => bytes.exportPayload(exp.index, exp.object_name),
      onReplace: () => bytes.replacePayload(exp.index),
      onDiscard: () => edits.session.dropDraft(key),
    };
  };
  const bulkDrafted = (index: number) =>
    edits.session.drafts[draftKey(bulkTarget(index))] !== undefined;
  const head =
    "sticky top-0 bg-card px-3 py-1.5 text-[10px] font-semibold uppercase text-muted-foreground";
  const cell = "px-3 py-1 font-mono text-[11px]";
  return (
    <div className="min-h-0 min-w-0 flex-1 overflow-auto text-[11px]">
      <div className="border-b border-border px-3 py-2 text-muted-foreground">
        <span className="font-mono text-foreground/80">{pkg.package_name}</span>
        {" · "}
        {pkg.cooked ? "cooked" : "uncooked"}
        {pkg.unversioned_properties ? ", unversioned properties" : ""}
        {" · "}
        {pkg.names.length} names, {pkg.imports.length} imports, {pkg.exports.length} exports
      </div>

      <div className="border-b border-border">
        <div className="px-3 pt-3 pb-1 text-xs font-semibold text-foreground">Imports</div>
        <table className="w-full table-fixed border-collapse">
          <thead>
            <tr className="text-left">
              <th className={cn(head, "w-14")}>#</th>
              <th className={cn(head, "w-64")}>Class</th>
              <th className={head}>Path</th>
              <th className={cn(head, "w-72")}>Used by</th>
              <th className={cn(head, "w-10")} />
            </tr>
          </thead>
          <tbody>
            {pkg.imports.map((info) => {
              const unused = importUnused(info.usage);
              return (
                <tr key={info.index} className="border-t border-border/30">
                  <td className={cn(cell, "text-muted-foreground")}>{info.index}</td>
                  <Tip content={`${info.class_package}.${info.class_name}`}>
                    <td className={cn(cell, "truncate")}>{info.class_name}</td>
                  </Tip>
                  <td className={cn(cell, "max-w-0")}>
                    <ImportPathCell info={info} edits={edits} />
                  </td>
                  <td className={cn(cell, "truncate font-sans text-muted-foreground")}>
                    {unused ? (
                      <Tip content="Nothing in this package names it, so the table can lose it.">
                        <span className="rounded bg-muted px-1 text-[10px] uppercase">unused</span>
                      </Tip>
                    ) : (
                      importUsageText(info.usage)
                    )}
                  </td>
                  <td className="px-1 py-0.5">
                    <Tip
                      content={
                        lock ??
                        (unused
                          ? "Take this row out of the import table."
                          : `Kept by ${importUsageText(info.usage)}. Removing it is refused.`)
                      }
                    >
                      <Button
                        size="sm"
                        variant="ghost"
                        className="h-6 px-1"
                        disabled={!!lock || !unused}
                        onClick={() => onDropImport(-info.index - 1)}
                      >
                        <Trash2 size={12} />
                      </Button>
                    </Tip>
                  </td>
                </tr>
              );
            })}
            {adds.map(([key, draft]) => (
              <tr key={key} className="border-t border-border/30">
                <td className={cn(cell, "text-blue-accent-foreground")}>new</td>
                <td className={cn(cell, "truncate")}>{draft.className ?? "Object"}</td>
                <td className={cn(cell, "flex items-center gap-2")}>
                  <span className="min-w-0 truncate rounded-sm bg-blue-accent/15 px-1 text-blue-accent-foreground">
                    {draft.path}
                  </span>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-6 px-1"
                    onClick={() => edits.dropImportDraft(key)}
                  >
                    <X size={12} />
                  </Button>
                </td>
                <td className={cn(cell, "font-sans text-muted-foreground")}>added</td>
                <td />
              </tr>
            ))}
          </tbody>
          <AddImportRow edits={edits} serial={adds.length} cell={cell} />
        </table>
      </div>

      <div className="border-b border-border">
        <div className="flex items-center gap-2 px-3 pt-3 pb-1">
          <span className="text-xs font-semibold text-foreground">Exports</span>
          {clipboard && (
            <Tip
              content={
                lock ??
                `Bring ${clipboard.path} in from ${clipboard.entry.split("/").pop()}, with everything under it.`
              }
            >
              <Button
                size="sm"
                variant="outline"
                className="ml-auto h-6 text-[11px]"
                disabled={!!lock}
                onClick={onPaste}
              >
                <Copy size={12} /> Paste {clipboard.name}…
              </Button>
            </Tip>
          )}
        </div>
        <table className="w-full border-collapse">
          <thead>
            <tr className="text-left">
              <th className={cn(head, "w-10")}>#</th>
              <th className={head}>Name</th>
              <th className={head}>Class</th>
              <th className={head}>Outer</th>
              <th className={head}>Flags</th>
              <th className={head}>Status</th>
              <th className={cn(head, "w-10")} />
            </tr>
          </thead>
          <tbody>
            {pkg.exports.map((exp) => (
              <ContextMenu key={exp.index}>
                <ContextMenuTrigger asChild>
                  <tr className="border-t border-border/30">
                    <td className={cn(cell, "text-muted-foreground")}>{exp.index + 1}</td>
                    <td className={cn(cell, "truncate")}>
                      <Tip content={exp.path}>
                        <button
                          className="truncate hover:underline"
                          onClick={() => onSelectExport(exp.index)}
                        >
                          {exp.object_name}
                        </button>
                      </Tip>
                    </td>
                    <Tip content={referencePath(pkg, exp.class_index)}>
                      <td className={cn(cell, "truncate")}>{exp.class_name}</td>
                    </Tip>
                    <td className={cn(cell, "truncate text-muted-foreground")}>
                      {exp.outer_index === 0
                        ? "(package)"
                        : referencePath(pkg, exp.outer_index).split(/[.:]/).pop()}
                    </td>
                    <Tip content={`0x${exp.object_flags.toString(16)}`}>
                      <td className={cn(cell, "text-muted-foreground")}>
                        {flagNames(exp.object_flags)}
                      </td>
                    </Tip>
                    <td className="px-3 py-1">
                      <StatusBadge status={exp.status} note={exp.note} undecoded={exp.undecoded} />
                    </td>
                    <td className="px-1 py-0.5">
                      <ExportActions
                        export={exp}
                        lock={lock}
                        duplicateLock={duplicateLockOf(exp, pkg, lock)}
                        payload={payloadActions(exp)}
                        onRemove={() => onRemove(exp.index)}
                        onReset={() => onReset(exp.index)}
                        onDuplicate={() => onDuplicate(exp.index)}
                        onRename={() => onRename(exp.index)}
                        onFlags={() => onFlags(exp.index)}
                        onRetype={() => onRetype(exp.index)}
                        onDeps={() => onDeps(exp.index)}
                      />
                    </td>
                  </tr>
                </ContextMenuTrigger>
                <ContextMenuContent>
                  <ContextMenuItem disabled={!!lock} onSelect={() => onRename(exp.index)}>
                    <Pencil size={14} />
                    Rename export…
                  </ContextMenuItem>
                  <ContextMenuItem disabled={!!lock} onSelect={() => onFlags(exp.index)}>
                    <Flag size={14} />
                    Edit flags…
                  </ContextMenuItem>
                  <ContextMenuItem disabled={!!lock} onSelect={() => onRetype(exp.index)}>
                    <Database size={14} />
                    Retype…
                  </ContextMenuItem>
                  <ContextMenuItem onSelect={() => onCopyOut(exp.index)}>
                    <Copy size={14} />
                    Copy for pasting
                  </ContextMenuItem>
                  <ContextMenuItem disabled={!!lock} onSelect={() => onDeps(exp.index)}>
                    <ArrowLeftRight size={14} />
                    Load order…
                  </ContextMenuItem>
                  <ContextMenuItem disabled={!!lock} onSelect={() => onRemove(exp.index)}>
                    <Trash2 size={14} />
                    Remove export…
                  </ContextMenuItem>
                  <ContextMenuItem
                    disabled={!!resetLockOf(exp, lock)}
                    onSelect={() => onReset(exp.index)}
                  >
                    <RotateCcw size={14} />
                    Reset to defaults…
                  </ContextMenuItem>
                  <ContextMenuItem
                    disabled={!!duplicateLockOf(exp, pkg, lock)}
                    onSelect={() => onDuplicate(exp.index)}
                  >
                    <Copy size={14} />
                    Duplicate export…
                  </ContextMenuItem>
                </ContextMenuContent>
              </ContextMenu>
            ))}
          </tbody>
        </table>
      </div>

      {pkg.resources.length > 0 && (
        <div className="border-b border-border">
          <div className="px-3 pt-3 pb-1 text-xs font-semibold text-foreground">
            Bulk data ({pkg.resources.length})
          </div>
          <p className="px-3 pb-2 text-[11px] text-muted-foreground">
            Payloads the package keeps outside its properties: texture mips, mesh buffers and the
            like. Bytes go out to a file and come back from one; nothing inside is decoded.
          </p>
          <table className="w-full border-collapse">
            <thead>
              <tr className="text-left">
                <th className={cn(head, "w-14")}>#</th>
                <th className={cn(head, "w-20")}>Where</th>
                <th className={cn(head, "w-28")}>Offset</th>
                <th className={cn(head, "w-28")}>Size</th>
                <th className={cn(head, "w-20")}>Flags</th>
                <th className={head}>Owner</th>
                <th className={cn(head, "w-44")} />
              </tr>
            </thead>
            <tbody>
              {pkg.resources.map((r) => {
                const drafted = bulkDrafted(r.index);
                const replaceLock = r.locked ?? lock;
                return (
                  <tr key={r.index} className="border-t border-border/40 hover:bg-muted/40">
                    <td className={cn(cell, "text-muted-foreground")}>{r.index}</td>
                    <td className={cell}>{r.placement}</td>
                    <td className={cn(cell, "text-muted-foreground")}>
                      0x{r.serial_offset.toString(16)}
                    </td>
                    <td className={cell}>{formatBytes(r.serial_size)}</td>
                    <td className={cn(cell, "text-muted-foreground")}>0x{r.flags.toString(16)}</td>
                    <td className={cn(cell, "truncate text-muted-foreground")}>
                      {r.owner !== undefined
                        ? (pkg.exports.find((e) => e.index === r.owner)?.object_name ??
                          `export ${r.owner}`)
                        : "(file)"}
                    </td>
                    <td className="px-1 py-0.5">
                      <div className="flex items-center gap-1">
                        <Tip content={r.locked ?? "Save this payload's bytes to a file."}>
                          <Button
                            size="sm"
                            variant="ghost"
                            className="h-6 px-1.5 text-[11px]"
                            disabled={!!r.locked}
                            onClick={() => bytes.exportBulk(r.index)}
                          >
                            <Save size={12} /> Export…
                          </Button>
                        </Tip>
                        <Tip
                          content={
                            replaceLock ??
                            (drafted
                              ? "A file is queued for this payload; save to write it."
                              : "Swap this payload for a file's bytes.")
                          }
                        >
                          <Button
                            size="sm"
                            variant="ghost"
                            className={cn(
                              "h-6 px-1.5 text-[11px]",
                              drafted && "text-blue-accent-foreground"
                            )}
                            disabled={!!replaceLock}
                            onClick={() =>
                              drafted
                                ? edits.session.dropDraft(draftKey(bulkTarget(r.index)))
                                : bytes.replaceBulk(r.index)
                            }
                          >
                            {drafted ? <Undo2 size={12} /> : <RefreshCw size={12} />}
                            {drafted ? "Discard" : "Replace…"}
                          </Button>
                        </Tip>
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      <details className="border-b border-border">
        <summary className="cursor-pointer px-3 py-2 text-xs font-semibold text-foreground hover:text-primary">
          Names ({pkg.names.length})
        </summary>
        <div className="grid grid-cols-[3rem_1fr] gap-x-3 px-3 pb-3 font-mono text-[11px]">
          {pkg.names.map((name, i) => (
            <React.Fragment key={i}>
              <span className="text-muted-foreground">{i}</span>
              <Tip content={name} disabled={name.length <= 48}>
                <span className="truncate">{name}</span>
              </Tip>
            </React.Fragment>
          ))}
        </div>
      </details>
    </div>
  );
}

/** Changes to the export table itself, offered for the export on show. */
/** What the payload buttons of an export need: whether it can be replaced and what is queued. */
interface PayloadActions {
  lock: string | null;
  drafted: boolean;
  onExport: () => void;
  onReplace: () => void;
  onDiscard: () => void;
}

/** Why an export cannot be copied, mirroring the backend's refusals as far as the view can see. */
function duplicateLockOf(
  exp: ParsedExport,
  pkg: ParsedPackage | null,
  lock: string | null
): string | null {
  if (lock) return lock;
  if (
    exp.class_name.endsWith("Class") ||
    /^(Function|ScriptStruct|UserDefinedStruct|Enum|UserDefinedEnum)$/.test(exp.class_name)
  ) {
    return "Classes, functions and structs carry layouts the reader only walks, so they are not copied.";
  }
  if ((exp.object_flags & 0x10) !== 0) {
    return "A class has exactly one default object.";
  }
  if (exp.status.state === "partial" || exp.status.state === "failed") {
    return "This export did not decode to its end, so its references could not be pointed at a copy.";
  }
  if (pkg?.resources.some((r) => r.owner === exp.index)) {
    return "This export holds inline bulk data, which the bulk table addresses by position.";
  }
  return null;
}

function ExportActions({
  export: exp,
  lock,
  duplicateLock,
  payload,
  onRemove,
  onReset,
  onDuplicate,
  onRename,
  onFlags,
  onRetype,
  onDeps,
}: {
  export: ParsedExport;
  lock: string | null;
  duplicateLock: string | null;
  payload: PayloadActions;
  onRemove: () => void;
  onReset: () => void;
  onDuplicate: () => void;
  onRename: () => void;
  onFlags: () => void;
  onRetype: () => void;
  onDeps: () => void;
}) {
  const [open, setOpen] = useState(false);
  const resetLock = resetLockOf(exp, lock);
  const item = "flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs";
  const payloadLock = payload.lock;
  const replaceLock = payloadLock ?? lock;
  const replaceHint =
    exp.status.state === "payload" && exp.status.kind === "bytecode"
      ? exp.object_name.startsWith("ExecuteUbergraph_")
        ? "Swap the event graph for a file of exactly the same length. The events call into it at fixed offsets that are not rewritten, and the indices inside have to come from this package's own layout."
        : "Swap the bytecode for a file's bytes. Bytecode that disassembles may take any length, and the size words before it follow; bytes that do not must keep the length they replace. The indices inside have to come from this package's own layout."
      : "Swap the payload for a file's bytes. Nothing inside is decoded, so the file has to be laid out the way this class expects.";
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <Tip content="Export actions">
        <PopoverTrigger asChild>
          <Button size="sm" variant="ghost" className="h-7 px-1.5">
            <MoreHorizontal size={14} />
          </Button>
        </PopoverTrigger>
      </Tip>
      <PopoverContent align="end" className="w-64 p-1">
        <Tip
          content={
            lock ??
            "Change the object's name. Every path naming it moves with it, including its subobjects'."
          }
          side="right"
        >
          <button
            className={cn(item, lock ? "cursor-not-allowed opacity-50" : "hover:bg-muted")}
            disabled={!!lock}
            onClick={() => {
              setOpen(false);
              onRename();
            }}
          >
            <Pencil size={14} /> Rename export…
          </button>
        </Tip>
        <Tip content={lock ?? "Change the flags the loader reads this object by."} side="right">
          <button
            className={cn(item, lock ? "cursor-not-allowed opacity-50" : "hover:bg-muted")}
            disabled={!!lock}
            onClick={() => {
              setOpen(false);
              onFlags();
            }}
          >
            <Flag size={14} /> Edit flags…
          </button>
        </Tip>
        <Tip
          content={
            lock ??
            "Change the order the loader builds this object in, relative to the rest of the package."
          }
          side="right"
        >
          <button
            className={cn(item, lock ? "cursor-not-allowed opacity-50" : "hover:bg-muted")}
            disabled={!!lock}
            onClick={() => {
              setOpen(false);
              onDeps();
            }}
          >
            <ArrowLeftRight size={14} /> Load order…
          </button>
        </Tip>
        <Tip
          content={
            lock ?? "Make the object another class. Its stored values do not survive the change."
          }
          side="right"
        >
          <button
            className={cn(item, lock ? "cursor-not-allowed opacity-50" : "hover:bg-muted")}
            disabled={!!lock}
            onClick={() => {
              setOpen(false);
              onRetype();
            }}
          >
            <Database size={14} /> Retype…
          </button>
        </Tip>
        <Tip
          content={
            duplicateLock ??
            "Copy this export and its subobjects to the end of the table under a new name."
          }
          side="right"
        >
          <button
            className={cn(item, duplicateLock ? "cursor-not-allowed opacity-50" : "hover:bg-muted")}
            disabled={!!duplicateLock}
            onClick={() => {
              setOpen(false);
              onDuplicate();
            }}
          >
            <Copy size={14} /> Duplicate export…
          </button>
        </Tip>
        <Tip
          content={
            payloadLock ??
            "Save the bytes after this export's properties to a file, for another tool to work on."
          }
          side="right"
        >
          <button
            className={cn(item, payloadLock ? "cursor-not-allowed opacity-50" : "hover:bg-muted")}
            disabled={!!payloadLock}
            onClick={() => {
              setOpen(false);
              payload.onExport();
            }}
          >
            <Save size={14} /> Export payload bytes…
          </button>
        </Tip>
        <Tip content={replaceLock ?? replaceHint} side="right">
          <button
            className={cn(item, replaceLock ? "cursor-not-allowed opacity-50" : "hover:bg-muted")}
            disabled={!!replaceLock}
            onClick={() => {
              setOpen(false);
              if (payload.drafted) payload.onDiscard();
              else payload.onReplace();
            }}
          >
            {payload.drafted ? <Undo2 size={14} /> : <RefreshCw size={14} />}
            {payload.drafted ? "Discard payload replacement" : "Replace payload bytes…"}
          </button>
        </Tip>
        <Tip
          content={lock ?? "Take this export, and any subobjects it owns, out of the package."}
          side="right"
        >
          <button
            className={cn(item, lock ? "cursor-not-allowed opacity-50" : "hover:bg-muted")}
            disabled={!!lock}
            onClick={() => {
              setOpen(false);
              onRemove();
            }}
          >
            <Trash2 size={14} /> Remove export…
          </button>
        </Tip>
        <Tip
          content={
            resetLock ?? "Drop every value this export stores so it inherits its class defaults."
          }
          side="right"
        >
          <button
            className={cn(item, resetLock ? "cursor-not-allowed opacity-50" : "hover:bg-muted")}
            disabled={!!resetLock}
            onClick={() => {
              setOpen(false);
              onReset();
            }}
          >
            <RotateCcw size={14} /> Reset to defaults…
          </button>
        </Tip>
      </PopoverContent>
    </Popover>
  );
}

/** Confirms a removal or reset, showing what the removal plan found before anything is written. */
function StructuralDialog({
  ask,
  pkg,
  modName,
  saveTarget,
  saving,
  indexing,
  onBuildIndex,
  onRenamePlan,
  onRetypePlan,
  onDependencyPlan,
  onPastePlan,
  onPaste,
  onClose,
  onConfirm,
}: {
  ask: StructuralAsk | null;
  pkg: ParsedPackage;
  modName: string;
  saveTarget: SaveTarget;
  saving: boolean;
  indexing: { current: number; total: number } | null;
  onBuildIndex: (index: number) => void;
  onRenamePlan: (index: number, name: string) => void;
  onRetypePlan: (index: number, target: number) => void;
  onDependencyPlan: (index: number, runs: DependencyRuns) => void;
  onPastePlan: (outer: number | null, name: string) => void;
  onPaste: (outer: number | null, name: string) => void;
  onClose: () => void;
  onConfirm: (structural: Structural) => void;
}) {
  // Every ask but these two is about one export, and names it by index.
  const target =
    ask && ask.kind !== "drop_import" && ask.kind !== "paste" ? pkg.exports[ask.index] : undefined;
  const pak = previewContainerFilename(modName, saveTarget);
  const plan = ask?.kind === "remove" ? ask.plan : null;
  const unsafe = plan !== null && plan.warnings.length > 0;
  const blocked = plan !== null && plan.blockers.length > 0;
  const list =
    "max-h-40 overflow-auto rounded-md border border-border bg-muted/40 p-2 font-mono text-[11px]";
  return (
    <AlertDialog open={ask !== null} onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent className="max-w-2xl">
        {ask?.kind === "remove" && target && (
          <>
            <AlertDialogHeader>
              <AlertDialogTitle className="flex items-center gap-2">
                Remove {target.object_name}?
                {unsafe && (
                  <span className="rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase text-amber-300">
                    Unsafe
                  </span>
                )}
              </AlertDialogTitle>
              <AlertDialogDescription>
                {pak
                  ? `Writes the package without it into ${pak}.`
                  : "Name a mod to save into first."}
              </AlertDialogDescription>
            </AlertDialogHeader>
            {ask.error && <p className="text-xs text-err">{ask.error}</p>}
            {!ask.error && !plan && (
              <p className="flex items-center gap-2 text-xs text-muted-foreground">
                <Loader2 size={13} className="animate-spin" /> Working out what the removal touches
              </p>
            )}
            {plan && (
              <div className="flex flex-col gap-3 text-xs">
                <div>
                  <p className="mb-1 text-muted-foreground">
                    {plan.removed.length === 1
                      ? "Removes one export."
                      : `Removes ${plan.removed.length} exports, subobjects included.`}
                    {plan.renumbered > 0
                      ? ` ${plan.renumbered} later export${plan.renumbered === 1 ? " is" : "s are"} renumbered.`
                      : " Nothing else is renumbered."}
                  </p>
                  <div className={list}>
                    {plan.removed.map((r) => (
                      <Tip key={r.index} content={r.path}>
                        <div className="truncate">
                          <span className="text-muted-foreground">[{r.index}]</span> {r.path}{" "}
                          <span className="text-muted-foreground">
                            {r.class_name}
                            {r.requested ? "" : ", subobject"}
                          </span>
                        </div>
                      </Tip>
                    ))}
                  </div>
                </div>
                {plan.cleared.length > 0 && (
                  <div>
                    <p className="mb-1 text-muted-foreground">
                      {plan.cleared.length === 1
                        ? "One reference to it is set to None:"
                        : `${plan.cleared.length} references to it are set to None:`}
                    </p>
                    <div className={list}>
                      {plan.cleared.map((c, i) => (
                        <div key={i} className="truncate">
                          {c.export_name}.{c.property}{" "}
                          <span className="text-muted-foreground">was {c.target}</span>
                        </div>
                      ))}
                    </div>
                  </div>
                )}
                {plan.blockers.map((b, i) => (
                  <p
                    key={i}
                    className="rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1.5 text-red-300"
                  >
                    {b}
                  </p>
                ))}
                {plan.warnings.map((w, i) => (
                  <p
                    key={i}
                    className="rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5 text-amber-300"
                  >
                    {w}
                  </p>
                ))}
                {plan.public.length > 0 &&
                  (plan.index_available ? (
                    <div>
                      <p className="mb-1 text-muted-foreground">
                        Packages importing the removed exports, from the import index:
                      </p>
                      <div className={list}>
                        {plan.importers.map((entry) => (
                          <div key={entry.path}>
                            <Tip content={entry.path}>
                              <div className="truncate">
                                {entry.path.split(/[.:]/).pop()}:{" "}
                                <span className="text-muted-foreground">
                                  {entry.packages.length === 0
                                    ? "no importer known"
                                    : `${entry.packages.length} package${entry.packages.length === 1 ? "" : "s"}`}
                                </span>
                              </div>
                            </Tip>
                            {entry.packages.slice(0, 20).map((p) => (
                              <Tip key={p} content={p}>
                                <div className="truncate pl-3 text-muted-foreground">{p}</div>
                              </Tip>
                            ))}
                            {entry.packages.length > 20 && (
                              <div className="pl-3 text-muted-foreground">
                                and {entry.packages.length - 20} more
                              </div>
                            )}
                          </div>
                        ))}
                      </div>
                    </div>
                  ) : (
                    <div className="flex items-center gap-3 text-muted-foreground">
                      <span className="min-w-0 flex-1">
                        An import index can name the packages that import the removed exports.
                        Building it reads every package once and takes a few minutes.
                      </span>
                      <Button
                        size="sm"
                        variant="outline"
                        className="h-7 shrink-0"
                        disabled={indexing !== null}
                        onClick={() => onBuildIndex(ask.index)}
                      >
                        {indexing && <Loader2 size={13} className="animate-spin" />}
                        {indexing
                          ? indexing.total > 0
                            ? `${indexing.current} / ${indexing.total}`
                            : "Opening containers…"
                          : "Build index"}
                      </Button>
                    </div>
                  ))}
              </div>
            )}
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                disabled={!plan || blocked || saving || !pak}
                onClick={() => onConfirm({ remove: [ask.index] })}
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                {unsafe ? "Remove anyway" : "Remove"}
              </AlertDialogAction>
            </AlertDialogFooter>
          </>
        )}
        {ask?.kind === "duplicate" && target && (
          <DuplicateForm
            key={ask.index}
            target={target}
            pkg={pkg}
            pak={pak}
            saving={saving}
            onConfirm={(name, intoLevel) =>
              onConfirm({
                duplicate: [{ export: ask.index, name, into_level: intoLevel }],
              })
            }
          />
        )}
        {ask?.kind === "rename" && target && (
          <RenameForm
            key={ask.index}
            target={target}
            pkg={pkg}
            pak={pak}
            saving={saving}
            plan={ask.plan}
            error={ask.error}
            onName={onRenamePlan}
            onConfirm={(name) =>
              onConfirm({ exports: [{ op: "rename", export: ask.index, name }] })
            }
          />
        )}
        {ask?.kind === "flags" && target && (
          <FlagsForm
            key={ask.index}
            target={target}
            pak={pak}
            saving={saving}
            onConfirm={(set, clear) =>
              onConfirm({ exports: [{ op: "set_flags", export: ask.index, set, clear }] })
            }
          />
        )}
        {ask?.kind === "retype" && target && (
          <RetypeForm
            key={ask.index}
            target={target}
            pkg={pkg}
            pak={pak}
            saving={saving}
            plan={ask.plan}
            error={ask.error}
            onClass={onRetypePlan}
            onConfirm={(index) =>
              onConfirm({
                exports: [{ op: "set_class", export: ask.index, class: index }],
                reset: [ask.index],
              })
            }
          />
        )}
        {ask?.kind === "deps" && target && (
          <DependencyForm
            key={ask.index}
            target={target}
            pkg={pkg}
            pak={pak}
            saving={saving}
            plan={ask.plan}
            error={ask.error}
            onRuns={onDependencyPlan}
            onConfirm={(runs) => onConfirm({ dependencies: [{ export: ask.index, runs }] })}
          />
        )}
        {ask?.kind === "paste" && (
          <PasteForm
            key={`${ask.held.container}:${ask.held.export}`}
            held={ask.held}
            pkg={pkg}
            pak={pak}
            saving={saving}
            plan={ask.plan}
            error={ask.error}
            onDraft={onPastePlan}
            onConfirm={onPaste}
          />
        )}
        {ask?.kind === "drop_import" && (
          <ImportRemovalForm
            key={ask.import}
            info={pkg.imports[ask.import]}
            pak={pak}
            saving={saving}
            plan={ask.plan}
            error={ask.error}
            onConfirm={() => onConfirm({ removeImports: [ask.import] })}
          />
        )}
        {ask?.kind === "reset" && target && (
          <>
            <AlertDialogHeader>
              <AlertDialogTitle>Reset {target.object_name} to its defaults?</AlertDialogTitle>
              <AlertDialogDescription>
                Drops the {target.properties.filter((p) => p.value.kind !== "unset").length} value
                {target.properties.filter((p) => p.value.kind !== "unset").length === 1
                  ? ""
                  : "s"}{" "}
                this export stores, so it takes every value from {target.class_name}. Anything the
                class writes after its properties stays.{" "}
                {pak ? `Writes the result into ${pak}.` : "Name a mod to save into first."}
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                disabled={saving || !pak}
                onClick={() => onConfirm({ reset: [ask.index] })}
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                Reset
              </AlertDialogAction>
            </AlertDialogFooter>
          </>
        )}
      </AlertDialogContent>
    </AlertDialog>
  );
}

/** Renames an export. The plan comes from the backend as the name is typed, since only it knows
 *  which paths move and which packages import them by a hash the move changes. */
function RenameForm({
  target,
  pkg,
  pak,
  saving,
  plan,
  error,
  onName,
  onConfirm,
}: {
  target: ParsedExport;
  pkg: ParsedPackage;
  pak: string | null;
  saving: boolean;
  plan: ExportEditPlan | null;
  error: string | null;
  onName: (index: number, name: string) => void;
  onConfirm: (name: string) => void;
}) {
  const [name, setName] = useState(target.object_name);
  const trimmed = name.trim();
  const changed = trimmed !== target.object_name;
  useEffect(() => {
    if (!changed || trimmed.length === 0) return;
    const timer = setTimeout(() => onName(target.index, trimmed), 250);
    return () => clearTimeout(timer);
  }, [changed, trimmed, target.index, onName]);
  const taken = pkg.exports.some(
    (exp) =>
      exp.index !== target.index &&
      exp.outer_index === target.outer_index &&
      exp.object_name.toLowerCase() === trimmed.toLowerCase()
  );
  const blocked = (plan?.blockers.length ?? 0) > 0;
  const valid = trimmed.length > 0 && changed && !taken && !/[.:/\\]/.test(trimmed);
  const list =
    "max-h-40 overflow-auto rounded-md border border-border bg-muted/40 p-2 font-mono text-[11px]";
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Rename {target.object_name}</AlertDialogTitle>
        <AlertDialogDescription>
          Moves the object and everything under it to a new path. Nothing inside the package is
          rewritten, but a package that imports this one by hash reads the path.{" "}
          {pak ? `Writes the result into ${pak}.` : "Name a mod to save into first."}
        </AlertDialogDescription>
      </AlertDialogHeader>
      <Input
        autoFocus
        value={name}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && valid && pak && !saving && !blocked) {
            e.preventDefault();
            onConfirm(trimmed);
          }
        }}
        placeholder="Object name"
        className="h-8 font-mono text-xs"
      />
      {taken && (
        <p className="text-[11px] text-destructive">
          There is already an object named {trimmed} beside {target.object_name}.
        </p>
      )}
      {error && <p className="text-xs text-err">{error}</p>}
      {valid && !error && !plan && (
        <p className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 size={13} className="animate-spin" /> Working out what the rename touches
        </p>
      )}
      {plan && valid && (
        <div className="flex flex-col gap-3 text-xs">
          {plan.repathed.length > 0 && (
            <div>
              <p className="mb-1 text-muted-foreground">
                {plan.repathed.length === 1
                  ? "One path moves:"
                  : `${plan.repathed.length} paths move:`}
              </p>
              <div className={list}>
                {plan.repathed.map(([was, now]) => (
                  <Tip key={was} content={`${was} to ${now}`}>
                    <div className="truncate">{now}</div>
                  </Tip>
                ))}
              </div>
            </div>
          )}
          {plan.blockers.map((b, i) => (
            <p
              key={i}
              className="rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1.5 text-red-300"
            >
              {b}
            </p>
          ))}
          {plan.warnings.map((w, i) => (
            <p
              key={i}
              className="rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5 text-amber-300"
            >
              {w}
            </p>
          ))}
        </div>
      )}
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction
          disabled={!valid || saving || !pak || blocked}
          onClick={() => onConfirm(trimmed)}
        >
          Rename
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

/** Ticks the flag bits the loader reads an export by. Bits outside the writable set are shown as
 *  they stand, because setting one is a change the loader would drop. */
function FlagsForm({
  target,
  pak,
  saving,
  onConfirm,
}: {
  target: ParsedExport;
  pak: string | null;
  saving: boolean;
  onConfirm: (set: number, clear: number) => void;
}) {
  const [flags, setFlags] = useState(target.object_flags);
  const editable = OBJECT_FLAGS.reduce((mask, [bit]) => mask | bit, 0);
  const other = target.object_flags & ~editable;
  const set = flags & ~target.object_flags;
  const clear = target.object_flags & ~flags;
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Flags on {target.object_name}</AlertDialogTitle>
        <AlertDialogDescription>
          Changes the row in the export table and nothing else.{" "}
          {pak ? `Writes the result into ${pak}.` : "Name a mod to save into first."}
        </AlertDialogDescription>
      </AlertDialogHeader>
      <div className="flex flex-col gap-1">
        {OBJECT_FLAGS.map(([bit, name]) => {
          const locked = bit === RF_CLASS_DEFAULT_OBJECT;
          return (
            <Tip
              key={bit}
              content={
                locked
                  ? "A class default object is the one its class names, so this bit is not editable."
                  : `0x${bit.toString(16)}`
              }
              side="right"
            >
              <label
                className={cn(
                  "flex items-center gap-2 rounded px-2 py-1 text-xs",
                  locked ? "opacity-50" : "cursor-pointer hover:bg-muted"
                )}
              >
                <input
                  type="checkbox"
                  disabled={locked}
                  checked={(flags & bit) !== 0}
                  onChange={(e) =>
                    setFlags((held) => (e.target.checked ? held | bit : held & ~bit))
                  }
                />
                <span className="font-mono">{name}</span>
              </label>
            </Tip>
          );
        })}
        {other !== 0 && (
          <p className="px-2 pt-1 text-[11px] text-muted-foreground">
            It also holds 0x{other.toString(16)}, which the loader drops on load and this editor
            leaves as it is.
          </p>
        )}
      </div>
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction
          disabled={(set === 0 && clear === 0) || saving || !pak}
          onClick={() => onConfirm(set, clear)}
        >
          Save flags
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

/** Brings an export in from another package: pick where it lands and what it is called. The plan
 *  comes from the backend, since only it can read the source and say what would come across. */
function PasteForm({
  held,
  pkg,
  pak,
  saving,
  plan,
  error,
  onDraft,
  onConfirm,
}: {
  held: ExportClipboard;
  pkg: ParsedPackage;
  pak: string | null;
  saving: boolean;
  plan: CopyPlan | null;
  error: string | null;
  onDraft: (outer: number | null, name: string) => void;
  onConfirm: (outer: number | null, name: string) => void;
}) {
  const [outer, setOuter] = useState<number | null>(null);
  const [name, setName] = useState(held.name);
  const trimmed = name.trim();
  useEffect(() => {
    if (trimmed.length === 0) return;
    const timer = setTimeout(() => onDraft(outer, trimmed), 300);
    return () => clearTimeout(timer);
  }, [outer, trimmed, onDraft]);
  const blocked = (plan?.blockers.length ?? 0) > 0;
  const valid = trimmed.length > 0 && !/[.:/\\]/.test(trimmed);
  const list =
    "max-h-40 overflow-auto rounded-md border border-border bg-muted/40 p-2 font-mono text-[11px]";
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Paste {held.name}</AlertDialogTitle>
        <AlertDialogDescription>
          Copies <span className="font-mono">{held.path}</span> and everything under it into this
          package. Its names, imports and references are rewritten against this package&apos;s
          tables. {pak ? `Writes the result into ${pak}.` : "Name a mod to save into first."}
        </AlertDialogDescription>
      </AlertDialogHeader>
      <div className="flex flex-col gap-2">
        <span className="text-[11px] text-muted-foreground">Put it under</span>
        <select
          value={outer ?? ""}
          onChange={(e) => setOuter(e.target.value === "" ? null : Number(e.target.value))}
          className="h-8 rounded-md border border-input bg-transparent px-2 font-mono text-xs"
        >
          <option value="">(the package root)</option>
          {pkg.exports.map((exp) => (
            <option key={exp.index} value={exp.index}>
              [{exp.index}] {exp.object_name} ({exp.class_name})
            </option>
          ))}
        </select>
        <span className="text-[11px] text-muted-foreground">Called</span>
        <Input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Object name"
          className="h-8 font-mono text-xs"
        />
      </div>
      {error && <p className="text-xs text-err">{error}</p>}
      {valid && !error && !plan && (
        <p className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 size={13} className="animate-spin" /> Reading the source package
        </p>
      )}
      {plan && plan.copies.length > 0 && (
        <div>
          <p className="mb-1 text-xs text-muted-foreground">
            {plan.copies.length === 1
              ? "Brings one export across:"
              : `Brings ${plan.copies.length} exports across:`}
          </p>
          <div className={list}>
            {plan.copies.map((copy) => (
              <Tip key={copy.export} content={copy.path}>
                <div className="truncate">
                  {copy.path.split(/[.:]/).pop()}{" "}
                  <span className="text-muted-foreground">
                    {copy.class_name}
                    {copy.requested ? "" : ", subobject"}
                  </span>
                </div>
              </Tip>
            ))}
          </div>
        </div>
      )}
      {plan?.blockers.map((b, i) => (
        <p
          key={i}
          className="rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1.5 text-xs text-red-300"
        >
          {b}
        </p>
      ))}
      {plan?.warnings.map((w, i) => (
        <p
          key={i}
          className="rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5 text-xs text-amber-300"
        >
          {w}
        </p>
      ))}
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction
          disabled={!valid || !plan || blocked || saving || !pak}
          onClick={() => onConfirm(outer, trimmed)}
        >
          Paste
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

/** Retypes an object: pick the class it becomes from the ones this package already names. Its
 *  stored values do not survive, so the dialog says so rather than letting it look reversible. */
function RetypeForm({
  target,
  pkg,
  pak,
  saving,
  plan,
  error,
  onClass,
  onConfirm,
}: {
  target: ParsedExport;
  pkg: ParsedPackage;
  pak: string | null;
  saving: boolean;
  plan: ExportEditPlan | null;
  error: string | null;
  onClass: (index: number, target: number) => void;
  onConfirm: (index: number) => void;
}) {
  // Every class this package already names, which is what a retype can point at without adding an
  // import first.
  const classes = useMemo(() => {
    const held = pkg.imports
      .filter((info) => info.class_name === "Class" || info.class_name.endsWith("GeneratedClass"))
      .map((info) => ({ index: info.index, name: info.object_name, path: info.path }));
    return held.sort((a, b) => a.name.localeCompare(b.name));
  }, [pkg.imports]);
  const [chosen, setChosen] = useState<number | null>(null);
  useEffect(() => {
    if (chosen === null) return;
    const timer = setTimeout(() => onClass(target.index, chosen), 250);
    return () => clearTimeout(timer);
  }, [chosen, target.index, onClass]);
  const stored = target.properties.filter((entry) => entry.value.kind !== "unset").length;
  const blocked = (plan?.blockers.length ?? 0) > 0;
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Retype {target.object_name}</AlertDialogTitle>
        <AlertDialogDescription>
          Its {stored} stored value{stored === 1 ? "" : "s"} do not survive: they were written under{" "}
          {target.class_name} and mean nothing under another class. The object is emptied and takes
          the new class&apos;s defaults.{" "}
          {pak ? `Writes the result into ${pak}.` : "Name a mod to save into first."}
        </AlertDialogDescription>
      </AlertDialogHeader>
      <select
        value={chosen ?? ""}
        onChange={(e) => setChosen(e.target.value === "" ? null : Number(e.target.value))}
        className="h-8 rounded-md border border-input bg-transparent px-2 font-mono text-xs"
      >
        <option value="">Pick a class this package names…</option>
        {classes.map((held) => (
          <option key={held.index} value={held.index}>
            {held.name}
          </option>
        ))}
      </select>
      {classes.length === 0 && (
        <p className="text-[11px] text-muted-foreground">
          This package names no class to retype to. Add an import for one first.
        </p>
      )}
      {error && <p className="text-xs text-err">{error}</p>}
      {chosen !== null && !error && !plan && (
        <p className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 size={13} className="animate-spin" /> Checking whether the retype reads back
        </p>
      )}
      {plan?.blockers.map((b, i) => (
        <p
          key={i}
          className="rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1.5 text-xs text-red-300"
        >
          {b}
        </p>
      ))}
      {plan?.warnings.map((w, i) => (
        <p
          key={i}
          className="rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5 text-xs text-amber-300"
        >
          {w}
        </p>
      ))}
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction
          disabled={chosen === null || !plan || blocked || saving || !pak}
          onClick={() => chosen !== null && onConfirm(chosen)}
          className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
        >
          Retype
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

/** The four runs, most useful first: a reference needs the create-before-serialize edge, and the
 *  rest are rarer. */
const RUNS: [keyof DependencyRuns, string, string][] = [
  [
    "create_before_serialize",
    "Create before serialize",
    "These objects must exist before this one's bytes are read, which is what a reference to one needs.",
  ],
  [
    "serialize_before_serialize",
    "Serialize before serialize",
    "These objects must be fully read before this one is.",
  ],
  [
    "create_before_create",
    "Create before create",
    "These objects must exist before this one is built.",
  ],
  [
    "serialize_before_create",
    "Serialize before create",
    "These objects must be fully read before this one is built, which is the strongest form.",
  ],
];

/** The four preload dependency runs, edited as comma-separated package indices. The plan comes
 *  from the backend as they are typed: only it knows whether the load order still walks. */
function DependencyForm({
  target,
  pkg,
  pak,
  saving,
  plan,
  error,
  onRuns,
  onConfirm,
}: {
  target: ParsedExport;
  pkg: ParsedPackage;
  pak: string | null;
  saving: boolean;
  plan: DependencyPlan | null;
  error: string | null;
  onRuns: (index: number, runs: DependencyRuns) => void;
  onConfirm: (runs: DependencyRuns) => void;
}) {
  const held = pkg.dependencies?.[target.index];
  const [text, setText] = useState<Record<string, string>>(() =>
    Object.fromEntries(RUNS.map(([key]) => [key, (held?.[key] ?? []).join(", ")]))
  );
  const parsed = useMemo((): DependencyRuns | null => {
    const out: Partial<DependencyRuns> = {};
    for (const [key] of RUNS) {
      const raw = (text[key] ?? "").trim();
      if (raw === "") {
        out[key] = [];
        continue;
      }
      const values = raw.split(",").map((part) => Number(part.trim()));
      if (values.some((v) => !Number.isInteger(v))) return null;
      out[key] = values;
    }
    return out as DependencyRuns;
  }, [text]);
  const changed =
    parsed !== null &&
    held !== undefined &&
    RUNS.some(([key]) => parsed[key].join() !== held[key].join());
  useEffect(() => {
    if (!changed || !parsed) return;
    const timer = setTimeout(() => onRuns(target.index, parsed), 300);
    return () => clearTimeout(timer);
  }, [changed, parsed, target.index, onRuns]);
  const blocked = (plan?.blockers.length ?? 0) > 0;
  const label = (index: number) =>
    index > 0
      ? (pkg.exports[index - 1]?.object_name ?? `export ${index - 1}`)
      : (pkg.imports.find((info) => info.index === index)?.class_name ?? `import ${-index - 1}`);
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Load order for {target.object_name}</AlertDialogTitle>
        <AlertDialogDescription>
          Package indices, comma separated: an export&apos;s position plus one, or minus an
          import&apos;s position plus one. Changing these changes the order the loader builds the
          package in and nothing else.{" "}
          {pak ? `Writes the result into ${pak}.` : "Name a mod to save into first."}
        </AlertDialogDescription>
      </AlertDialogHeader>
      {!held && (
        <p className="text-xs text-err">
          This package&apos;s dependency table did not read, so its runs cannot be changed.
        </p>
      )}
      <div className="flex flex-col gap-2">
        {RUNS.map(([key, name, hint]) => (
          <div key={key} className="flex flex-col gap-1">
            <Tip content={hint} side="right">
              <span className="w-fit text-[11px] text-muted-foreground">{name}</span>
            </Tip>
            <Input
              value={text[key] ?? ""}
              onChange={(e) => setText((prev) => ({ ...prev, [key]: e.target.value }))}
              placeholder="none"
              className="h-7 font-mono text-[11px]"
            />
            <span className="truncate text-[10px] text-muted-foreground">
              {(parsed?.[key] ?? []).map(label).join(", ")}
            </span>
          </div>
        ))}
      </div>
      {parsed === null && (
        <p className="text-[11px] text-destructive">
          These runs hold something that is not a whole number.
        </p>
      )}
      {error && <p className="text-xs text-err">{error}</p>}
      {changed && !error && !plan && (
        <p className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 size={13} className="animate-spin" /> Checking the load order
        </p>
      )}
      {plan?.blockers.map((b, i) => (
        <p
          key={i}
          className="rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1.5 text-xs text-red-300"
        >
          {b}
        </p>
      ))}
      {plan?.warnings.map((w, i) => (
        <p
          key={i}
          className="rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5 text-xs text-amber-300"
        >
          {w}
        </p>
      ))}
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction
          disabled={!changed || !parsed || saving || !pak || blocked}
          onClick={() => parsed && onConfirm(parsed)}
        >
          Save load order
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

/** Confirms dropping one import row, with what the backend found standing in the way. */
function ImportRemovalForm({
  info,
  pak,
  saving,
  plan,
  error,
  onConfirm,
}: {
  info: ImportInfo | undefined;
  pak: string | null;
  saving: boolean;
  plan: ImportRemovalPlan | null;
  error: string | null;
  onConfirm: () => void;
}) {
  const blocked = (plan?.blockers.length ?? 0) > 0;
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Remove import {info?.class_name}?</AlertDialogTitle>
        <AlertDialogDescription>
          <span className="font-mono">{info?.path}</span>
          {". "}
          {pak
            ? `Every import below it moves up a place. Writes the result into ${pak}.`
            : "Name a mod to save into first."}
        </AlertDialogDescription>
      </AlertDialogHeader>
      {error && <p className="text-xs text-err">{error}</p>}
      {!error && !plan && (
        <p className="flex items-center gap-2 text-xs text-muted-foreground">
          <Loader2 size={13} className="animate-spin" /> Working out what the removal touches
        </p>
      )}
      {plan && (
        <div className="flex flex-col gap-3 text-xs">
          <p className="text-muted-foreground">
            {plan.renumbered > 0
              ? `${plan.renumbered} import${plan.renumbered === 1 ? "" : "s"} move up a place.`
              : "Nothing is renumbered."}
            {plan.dropped_dependencies > 0
              ? ` ${plan.dropped_dependencies} preload dependency entr${plan.dropped_dependencies === 1 ? "y is" : "ies are"} dropped.`
              : ""}
          </p>
          {plan.blockers.map((b, i) => (
            <p
              key={i}
              className="rounded-md border border-red-500/30 bg-red-500/10 px-2 py-1.5 text-red-300"
            >
              {b}
            </p>
          ))}
          {plan.warnings.map((w, i) => (
            <p
              key={i}
              className="rounded-md border border-amber-500/30 bg-amber-500/10 px-2 py-1.5 text-amber-300"
            >
              {w}
            </p>
          ))}
        </div>
      )}
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction
          disabled={!plan || blocked || saving || !pak}
          onClick={onConfirm}
          className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
        >
          Remove
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

/** Names the copy of an export. The subobjects come along; the name has to be free beside it. */
function DuplicateForm({
  target,
  pkg,
  pak,
  saving,
  onConfirm,
}: {
  target: ParsedExport;
  pkg: ParsedPackage;
  pak: string | null;
  saving: boolean;
  onConfirm: (name: string, intoLevel?: number) => void;
}) {
  const [name, setName] = useState(`${target.object_name}_Copy`);
  // A level lists only the actors it owns, so the offer stands exactly when this export is one.
  const level = useMemo(() => {
    if (target.outer_index <= 0) return null;
    const outer = pkg.exports[target.outer_index - 1];
    if (!outer || outer.class_name !== "Level") return null;
    const actors = outer.properties.filter((entry) => entry.name === "Actors").pop();
    return actors?.span ? outer : null;
  }, [pkg.exports, target.outer_index]);
  const [listed, setListed] = useState(true);
  const trimmed = name.trim();
  const subobjects = useMemo(() => {
    const set = new Set<number>([target.index]);
    let grew = true;
    while (grew) {
      grew = false;
      for (const exp of pkg.exports) {
        if (!set.has(exp.index) && exp.outer_index > 0 && set.has(exp.outer_index - 1)) {
          set.add(exp.index);
          grew = true;
        }
      }
    }
    return set.size - 1;
  }, [pkg.exports, target.index]);
  const taken = pkg.exports.some(
    (exp) =>
      exp.outer_index === target.outer_index &&
      exp.object_name.toLowerCase() === trimmed.toLowerCase()
  );
  const valid = trimmed.length > 0 && !taken && !/[.:/\\]/.test(trimmed);
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Duplicate {target.object_name}</AlertDialogTitle>
        <AlertDialogDescription>
          Copies this export
          {subobjects > 0
            ? ` and its ${subobjects} subobject${subobjects === 1 ? "" : "s"}`
            : ""}{" "}
          to the end of the export table. References between the copies point at each other; nothing
          else points at the copy until you edit something to.{" "}
          {pak ? `Writes the result into ${pak}.` : "Name a mod to save into first."}
        </AlertDialogDescription>
      </AlertDialogHeader>
      <Input
        autoFocus
        value={name}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && valid && pak && !saving) {
            e.preventDefault();
            onConfirm(trimmed);
          }
        }}
        placeholder="Object name"
        className="h-8 font-mono text-xs"
      />
      {taken && (
        <p className="text-[11px] text-destructive">
          There is already an object named {trimmed} beside {target.object_name}.
        </p>
      )}
      {level && (
        <Tip
          content="An actor the level does not name loads with the package and is never spawned, so it would not appear in the world."
          side="right"
        >
          <label className="flex cursor-pointer items-center gap-2 rounded px-1 py-1 text-xs hover:bg-muted">
            <input type="checkbox" checked={listed} onChange={(e) => setListed(e.target.checked)} />
            Add the copy to {level.object_name}&apos;s actors
          </label>
        </Tip>
      )}
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction
          disabled={!valid || saving || !pak}
          onClick={() => onConfirm(trimmed, level && listed ? level.index : undefined)}
        >
          Duplicate
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

/** The notice and the pending changes, shared by every view of the asset. */
function EditBar({
  edits,
  gamePath,
  gameRunning,
  onOpenCopy,
}: {
  edits: AssetEdits;
  gamePath: string;
  gameRunning: boolean;
  onOpenCopy?: (container: string) => void;
}) {
  const preview = previewContainerFilename(edits.modName, edits.saveTarget);
  const suggestions = useModNameSuggestions(gamePath);
  const savedPak = edits.notice?.pak;
  return (
    <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
      {edits.notice && (
        <span
          className={cn(
            "flex min-w-0 items-center gap-1.5 truncate text-[11px]",
            edits.notice.type === "ok" ? "text-ok" : "text-err"
          )}
        >
          {edits.notice.type === "ok" ? <Check size={13} /> : <X size={13} />}
          {edits.notice.msg}
        </span>
      )}
      {savedPak && onOpenCopy && !edits.dirty && (
        <Tip content="The inspector shows the original asset. Open the copy the mod holds to build on what was saved.">
          <Button
            size="sm"
            variant="outline"
            className="h-7 shrink-0"
            onClick={() => onOpenCopy(savedPak)}
          >
            Open the saved copy
          </Button>
        </Tip>
      )}
      {edits.dirty && (
        <>
          {gameRunning ? (
            <span className="flex items-center gap-1.5 text-[11px] font-medium text-warn">
              <AlertTriangle size={13} className="shrink-0" /> Close the game to save changes
            </span>
          ) : (
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {edits.count} unsaved change{edits.count === 1 ? "" : "s"}
            </span>
          )}
          <span className="ml-auto flex min-w-0 items-center gap-2">
            <span className="shrink-0 text-[11px] text-muted-foreground">Mod</span>
            <Tip
              content={preview ? `Saves as ${preview} in ~mods` : "Name the mod pak to save into"}
            >
              <Input
                value={edits.modName}
                onChange={(e) => edits.setModName(e.target.value)}
                placeholder="AssetEdits"
                list="asset-mod-names"
                className="h-7 w-36 font-mono text-[11px]"
              />
            </Tip>
            <datalist id="asset-mod-names">
              {suggestions.map((name) => (
                <option key={name} value={name} />
              ))}
            </datalist>
            <Tip
              content={
                edits.saveTarget === "io_store"
                  ? "Writes an IoStore container, which is the only form the game loads packages from."
                  : "Writes a plain pak. The game does not load packages from one; this is for tooling that converts it onward itself."
              }
            >
              <select
                value={edits.saveTarget}
                onChange={(e) => edits.setSaveTarget(e.target.value as SaveTarget)}
                className="h-7 shrink-0 rounded-md border border-input bg-transparent px-2 text-[11px]"
              >
                <option value="io_store">IoStore</option>
                <option value="pak">Pak</option>
              </select>
            </Tip>
            <span className="hidden max-w-[240px] truncate text-[10px] text-muted-foreground xl:inline">
              {preview}
            </span>
            <Button
              variant="outline"
              size="sm"
              className="h-7"
              onClick={edits.discard}
              disabled={edits.saving}
            >
              <Undo2 size={13} /> Discard
            </Button>
            <Button
              variant="blue"
              size="sm"
              className="h-7"
              onClick={() => void edits.save()}
              disabled={edits.saving || gameRunning || !preview}
            >
              {edits.saving ? <RefreshCw size={13} className="animate-spin" /> : <Save size={13} />}
              {edits.saving ? "Saving…" : "Save as mod"}
            </Button>
          </span>
        </>
      )}
    </div>
  );
}

type DataTableRow = DataTable["rows"][number];

/** What the grid lays out: a row the table holds, or one that exists only as a pending draft. */
type GridRow =
  | { kind: "row"; row: DataTableRow }
  | { kind: "ghost"; name: string; source?: string };

/** A row operation waiting for its name. `at` is a position in the table as it was read. */
type RowNaming =
  | { kind: "add"; at: number }
  | { kind: "duplicate"; source: string; at: number }
  | { kind: "rename"; source: string };

/** The row-name cell, which is where a table's rows are added, copied, renamed and removed. */
function RowNameCell({
  row,
  position,
  draft,
  width,
  locked,
  onAdd,
  onDuplicate,
  onRename,
  onRemove,
  onDiscard,
}: {
  row: DataTableRow;
  position: number;
  draft: Draft | undefined;
  width: number;
  locked: string | null;
  onAdd: (at: number) => void;
  onDuplicate: (at: number) => void;
  onRename: () => void;
  onRemove: () => void;
  onDiscard: () => void;
}) {
  const removed = draft?.op === "row_remove";
  const renamed = draft?.op === "row_rename" ? draft.to : null;
  const cell = (
    <div
      className={cn(
        "shrink-0 truncate px-2 py-1 font-mono text-foreground/80",
        removed && "line-through",
        renamed !== null && "bg-blue-accent/15 text-blue-accent-foreground"
      )}
      style={{ width }}
    >
      {renamed ?? row.name}
    </div>
  );
  const hint = removed
    ? "This row is removed when you save."
    : renamed !== null
      ? `Renamed from ${row.name}`
      : null;
  const trigger = <ContextMenuTrigger asChild>{cell}</ContextMenuTrigger>;
  return (
    <ContextMenu>
      {hint ? <Tip content={hint}>{trigger}</Tip> : trigger}
      <ContextMenuContent>
        <ContextMenuItem disabled={!!locked} onSelect={() => onAdd(position)}>
          <Plus size={14} />
          Add row above
        </ContextMenuItem>
        <ContextMenuItem disabled={!!locked} onSelect={() => onAdd(position + 1)}>
          <Plus size={14} />
          Add row below
        </ContextMenuItem>
        <ContextMenuItem disabled={!!locked || removed} onSelect={() => onDuplicate(position + 1)}>
          <Copy size={14} />
          Duplicate row
        </ContextMenuItem>
        <ContextMenuItem disabled={!!locked || removed} onSelect={onRename}>
          <Pencil size={14} />
          Rename row
        </ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem
          disabled={!!locked || removed}
          onSelect={onRemove}
          className="text-destructive focus:text-destructive"
        >
          <Trash2 size={14} />
          Remove row
        </ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem disabled={draft === undefined} onSelect={onDiscard}>
          <Undo2 size={14} />
          Discard row change
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
}

/** A row that exists only as a draft. It has no bytes until the save, so nothing in it can be
 *  edited yet. */
function GhostRow({
  name,
  source,
  nameWidth,
  style,
  onDiscard,
}: {
  name: string;
  source?: string;
  nameWidth: number;
  style: React.CSSProperties;
  onDiscard: () => void;
}) {
  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <div
          className="absolute left-0 flex border-b border-dashed border-blue-accent/40 bg-blue-accent/5 text-[11px]"
          style={style}
        >
          <div
            className="flex shrink-0 items-center gap-1 truncate px-2 py-1 font-mono text-blue-accent-foreground"
            style={{ width: nameWidth }}
          >
            <Plus size={11} className="shrink-0" />
            <span className="truncate">{name}</span>
          </div>
          <div className="truncate px-2 py-1 italic text-muted-foreground">
            {source
              ? `A copy of ${source}. `
              : "A new row: every column reads the row struct's default. "}
            Save, then edit its values.
          </div>
        </div>
      </ContextMenuTrigger>
      <ContextMenuContent>
        <ContextMenuItem onSelect={onDiscard}>
          <Undo2 size={14} />
          Discard row change
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
}

/** Asks for a row name. Names are unique within a table and compared without regard to case. */
function RowNameForm({
  naming,
  taken,
  onConfirm,
}: {
  naming: RowNaming;
  taken: Set<string>;
  onConfirm: (name: string) => void;
}) {
  const [name, setName] = useState(naming.kind === "rename" ? naming.source : "");
  const trimmed = name.trim();
  const unchanged = naming.kind === "rename" && trimmed === naming.source;
  const clash = !unchanged && taken.has(trimmed.toLowerCase());
  const valid = trimmed.length > 0 && !clash;
  const title =
    naming.kind === "add"
      ? "Add a row"
      : naming.kind === "duplicate"
        ? `Duplicate ${naming.source}`
        : `Rename ${naming.source}`;
  const description =
    naming.kind === "add"
      ? "The new row stores nothing, so every column reads the row struct's default until you edit it after saving."
      : naming.kind === "duplicate"
        ? "The copy takes every value the source row stores."
        : "Other assets refer to rows by name, so to them a renamed row is a different row.";
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>{title}</AlertDialogTitle>
        <AlertDialogDescription>{description}</AlertDialogDescription>
      </AlertDialogHeader>
      <Input
        autoFocus
        value={name}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && valid) {
            e.preventDefault();
            onConfirm(trimmed);
          }
        }}
        placeholder="Row name"
        className="h-8 font-mono text-xs"
      />
      {clash && (
        <p className="text-[11px] text-destructive">
          This table already has a row named {trimmed}.
        </p>
      )}
      <AlertDialogFooter>
        <AlertDialogCancel>Cancel</AlertDialogCancel>
        <AlertDialogAction disabled={!valid} onClick={() => onConfirm(trimmed)}>
          {naming.kind === "add" ? "Add" : naming.kind === "duplicate" ? "Duplicate" : "Rename"}
        </AlertDialogAction>
      </AlertDialogFooter>
    </>
  );
}

/** What the strings grid lays out: an entry the table holds, or one that exists only as a draft. */
type StringRow =
  | {
      kind: "entry";
      index: number;
      key: string;
      source: string;
      tag: string;
      metadata: [string, string][];
    }
  | { kind: "ghost"; key: string; source: string };

/** A metadata item as the dialog shows it: the value the table holds, and what a draft makes it. */
interface MetaRow {
  id: string;
  held: string | null;
  draft: { op: "string_meta_set"; to: string } | { op: "string_meta_remove" } | null;
}

/** The metadata items of one entry, edited in place. Each change is a draft of its own, keyed by
 *  the item id, so several items can change in one save. */
function MetaDataForm({
  entry,
  rows,
  onSet,
  onRemove,
  onDiscard,
  locked,
}: {
  entry: string;
  rows: MetaRow[];
  onSet: (id: string, to: string) => void;
  onRemove: (id: string) => void;
  onDiscard: (id: string) => void;
  locked: string | null;
}) {
  const [newId, setNewId] = useState("");
  const [newValue, setNewValue] = useState("");
  const newIdTrimmed = newId.trim();
  const clash = rows.some((row) => row.id === newIdTrimmed);
  const canAdd = newIdTrimmed.length > 0 && !clash && !locked;
  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Metadata of {entry}</AlertDialogTitle>
        <AlertDialogDescription>
          Name and value pairs the table keeps beside this entry. Changes queue as drafts and land
          when you save.
        </AlertDialogDescription>
      </AlertDialogHeader>
      <div className="flex max-h-72 flex-col gap-1 overflow-auto text-[11px]">
        {rows.length === 0 && (
          <p className="text-muted-foreground">This entry has no metadata yet.</p>
        )}
        {rows.map((row) => {
          const removed = row.draft?.op === "string_meta_remove";
          const shown = row.draft?.op === "string_meta_set" ? row.draft.to : (row.held ?? "");
          return (
            <div key={row.id} className="flex items-center gap-2">
              <span
                className={cn(
                  "w-32 shrink-0 truncate font-mono",
                  removed && "line-through opacity-60",
                  row.held === null && "text-blue-accent-foreground"
                )}
              >
                {row.id}
              </span>
              <Input
                key={shown}
                defaultValue={shown}
                disabled={!!locked || removed}
                onBlur={(e) => {
                  if (e.target.value !== shown) onSet(row.id, e.target.value);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                }}
                className={cn(
                  "h-7 min-w-0 flex-1 font-mono text-[11px]",
                  row.draft?.op === "string_meta_set" && "ring-1 ring-blue-accent/60"
                )}
              />
              {row.draft ? (
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 shrink-0"
                  onClick={() => onDiscard(row.id)}
                >
                  <Undo2 size={13} />
                </Button>
              ) : (
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 shrink-0 text-destructive"
                  disabled={!!locked}
                  onClick={() => onRemove(row.id)}
                >
                  <Trash2 size={13} />
                </Button>
              )}
            </div>
          );
        })}
      </div>
      <div className="flex items-center gap-2 border-t border-border pt-2 text-[11px]">
        <Tip content={clash ? `This entry already has ${newIdTrimmed}.` : "The new item's id."}>
          <Input
            value={newId}
            onChange={(e) => setNewId(e.target.value)}
            placeholder="Id"
            className={cn("h-7 w-32 font-mono text-[11px]", clash && "ring-1 ring-destructive")}
          />
        </Tip>
        <Input
          value={newValue}
          onChange={(e) => setNewValue(e.target.value)}
          placeholder="Value"
          className="h-7 min-w-0 flex-1 font-mono text-[11px]"
        />
        <Button
          size="sm"
          variant="outline"
          className="h-7"
          disabled={!canAdd}
          onClick={() => {
            onSet(newIdTrimmed, newValue);
            setNewId("");
            setNewValue("");
          }}
        >
          <Plus size={13} /> Add
        </Button>
      </div>
      <AlertDialogFooter>
        <AlertDialogCancel>Done</AlertDialogCancel>
      </AlertDialogFooter>
    </>
  );
}

/** A StringTable's entries as a key and source grid. Cells edit in place; entries come and go
 *  through the row menu and the add row, and land only when saved. */
function StringTableView({ table, exportIndex }: { table: StringTable; exportIndex: number }) {
  const session = useEditSession();
  const [filter, setFilter] = useState("");
  const [editing, setEditing] = useState<string | null>(null);
  const [newKey, setNewKey] = useState("");
  const [newSource, setNewSource] = useState("");
  // Which entry's metadata dialog is open, and whether the tag column was asked for on a table
  // whose entries carry none.
  const [metaAsk, setMetaAsk] = useState<{ index: number; key: string } | null>(null);
  const [tagsWanted, setTagsWanted] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);

  const draftsHere = useMemo(() => {
    const byKey = new Map<string, DraftRecord>();
    for (const record of Object.values(session.drafts)) {
      if (record.target.string?.export === exportIndex) byKey.set(draftKey(record.target), record);
    }
    return byKey;
  }, [session.drafts, exportIndex]);
  const draftOf = useCallback(
    (index: number, key: string, field?: string) =>
      draftsHere.get(draftKey(stringTarget(exportIndex, index, key, field))),
    [draftsHere, exportIndex]
  );
  /** Every draft on one entry: its own, its tag's and its metadata items'. */
  const dropEntryDrafts = useCallback(
    (index: number) => {
      for (const [key, record] of draftsHere) {
        if (record.target.string?.index === index) session.dropDraft(key);
      }
    },
    [draftsHere, session]
  );

  const rows = useMemo<StringRow[]>(() => {
    const needle = filter.trim().toLowerCase();
    const out: StringRow[] = [];
    table.entries.forEach((entry, index) => {
      if (
        !needle ||
        entry.key.toLowerCase().includes(needle) ||
        entry.source.toLowerCase().includes(needle)
      ) {
        out.push({
          kind: "entry",
          index,
          key: entry.key,
          source: entry.source,
          tag: entry.tag,
          metadata: entry.metadata,
        });
      }
    });
    for (const record of draftsHere.values()) {
      if (record.draft.op === "string_add" && record.target.string) {
        out.push({ kind: "ghost", key: record.target.string.key, source: record.draft.source });
      }
    }
    return out;
  }, [table.entries, filter, draftsHere]);

  // Keys the table will hold once the drafts land, for refusing a duplicate before the backend has
  // to. Keys are case-sensitive here, unlike row names.
  const taken = useMemo(() => {
    const keys = new Set<string>();
    table.entries.forEach((entry, index) => {
      const draft = draftOf(index, entry.key)?.draft;
      if (draft?.op === "string_remove") return;
      keys.add(draft?.op === "string_set_key" ? draft.to : entry.key);
    });
    for (const record of draftsHere.values()) {
      if (record.draft.op === "string_add" && record.target.string)
        keys.add(record.target.string.key);
    }
    return keys;
  }, [table.entries, draftOf, draftsHere]);

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 26,
    overscan: 20,
  });

  const keyWidth = useMemo(
    () =>
      autoWidth(table.entries.slice(0, 200).reduce((w, entry) => Math.max(w, entry.key.length), 6)),
    [table.entries]
  );
  // The marker string after each source is empty in nearly every table, so the column only shows
  // where it says something, or once someone asks to set one.
  const hasTags =
    useMemo(() => table.entries.some((entry) => entry.tag !== ""), [table.entries]) ||
    tagsWanted ||
    [...draftsHere.values()].some((record) => record.draft.op === "string_set_tag");
  const addKeyTrimmed = newKey.trim();
  const addClash = taken.has(addKeyTrimmed);
  const canAdd = addKeyTrimmed.length > 0 && !addClash && !session.locked;

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
        <Table2 size={13} className="shrink-0 text-muted-foreground" />
        <span className="min-w-0 truncate text-xs text-muted-foreground">
          {rows.filter((row) => row.kind === "entry").length === table.entries.length
            ? `${table.entries.length} entries`
            : `${rows.filter((row) => row.kind === "entry").length} of ${table.entries.length} entries`}{" "}
          in <span className="font-mono text-foreground/80">{table.namespace}</span>
        </span>
        <Input
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter entries"
          className="ml-auto h-7 w-28 min-w-0 flex-1 text-xs sm:max-w-56"
        />
      </div>

      <div ref={scrollRef} className="min-h-0 min-w-0 flex-1 overflow-auto">
        <div className="sticky top-0 z-10 flex border-b border-border bg-card text-[10px] font-semibold uppercase text-muted-foreground">
          <div className="w-12 shrink-0 px-2 py-1">#</div>
          <div className="shrink-0 px-2 py-1" style={{ width: keyWidth }}>
            Key
          </div>
          <div className="min-w-0 flex-1 px-2 py-1">Source string</div>
          {hasTags && <div className="w-24 shrink-0 px-2 py-1">Tag</div>}
        </div>
        <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((virtual) => {
            const row = rows[virtual.index];
            const style = { top: virtual.start, height: virtual.size } as const;
            if (row.kind === "ghost") {
              const target = stringTarget(exportIndex, null, row.key);
              return (
                <ContextMenu key={virtual.key}>
                  <ContextMenuTrigger asChild>
                    <div
                      className="absolute left-0 right-0 flex border-b border-dashed border-blue-accent/40 bg-blue-accent/5 text-[11px]"
                      style={style}
                    >
                      <div className="flex w-12 shrink-0 items-center px-2 py-1 text-blue-accent-foreground">
                        <Plus size={11} />
                      </div>
                      <div
                        className="shrink-0 truncate px-2 py-1 font-mono text-blue-accent-foreground"
                        style={{ width: keyWidth }}
                      >
                        {row.key}
                      </div>
                      <div className="min-w-0 flex-1 truncate px-2 py-1 font-mono text-blue-accent-foreground">
                        {row.source}
                      </div>
                      {hasTags && <div className="w-24 shrink-0" />}
                    </div>
                  </ContextMenuTrigger>
                  <ContextMenuContent>
                    <ContextMenuItem onSelect={() => session.dropDraft(draftKey(target))}>
                      <Undo2 size={14} />
                      Discard added entry
                    </ContextMenuItem>
                  </ContextMenuContent>
                </ContextMenu>
              );
            }
            const target = stringTarget(exportIndex, row.index, row.key);
            const tagTarget = stringTarget(exportIndex, row.index, row.key, "tag");
            const key = draftKey(target);
            const draft = draftOf(row.index, row.key)?.draft;
            const tagDraft = draftOf(row.index, row.key, "tag")?.draft;
            const hasMetaDrafts = [...draftsHere.values()].some(
              (record) =>
                record.target.string?.index === row.index &&
                (record.draft.op === "string_meta_set" || record.draft.op === "string_meta_remove")
            );
            const anyDraft = draft !== undefined || tagDraft !== undefined || hasMetaDrafts;
            const removed = draft?.op === "string_remove";
            const shownKey = draft?.op === "string_set_key" ? draft.to : row.key;
            const shownSource = draft?.op === "string_set_source" ? draft.to : row.source;
            const shownTag = tagDraft?.op === "string_set_tag" ? tagDraft.to : row.tag;
            const widthOfField = (field: "key" | "source" | "tag") =>
              field === "key" ? { width: keyWidth } : undefined;
            const classOfField = (field: "key" | "source" | "tag") =>
              field === "key" ? "shrink-0" : field === "tag" ? "w-24 shrink-0" : "min-w-0 flex-1";
            const cell = (field: "key" | "source" | "tag") => {
              const id = `${key}${field}`;
              const text = field === "key" ? shownKey : field === "source" ? shownSource : shownTag;
              const changed =
                field === "key"
                  ? draft?.op === "string_set_key"
                  : field === "source"
                    ? draft?.op === "string_set_source"
                    : tagDraft?.op === "string_set_tag";
              const locked = session.locked ?? (removed ? "This entry is being removed." : null);
              if (editing === id && !locked) {
                return (
                  <div
                    className={cn("px-0.5 py-0.5", classOfField(field))}
                    style={widthOfField(field)}
                  >
                    <CellInput
                      initial={text}
                      onCommit={(next) => {
                        setEditing(null);
                        const original =
                          field === "key" ? row.key : field === "source" ? row.source : row.tag;
                        if (next === original) {
                          if (changed)
                            session.dropDraft(field === "tag" ? draftKey(tagTarget) : key);
                          return;
                        }
                        if (field === "key" && (next.trim() === "" || taken.has(next.trim()))) {
                          return;
                        }
                        if (field === "tag") {
                          session.setDraft(
                            tagTarget,
                            { op: "string_set_tag", to: next.trim() },
                            []
                          );
                          return;
                        }
                        session.setDraft(
                          target,
                          field === "key"
                            ? { op: "string_set_key", to: next.trim() }
                            : { op: "string_set_source", to: next },
                          []
                        );
                      }}
                      onCancel={() => setEditing(null)}
                    />
                  </div>
                );
              }
              const body = (
                <div
                  onClick={(e) => {
                    if (locked) return;
                    e.stopPropagation();
                    setEditing(id);
                  }}
                  className={cn(
                    "truncate px-2 py-1 font-mono",
                    classOfField(field),
                    field === "tag" && !changed && "text-muted-foreground",
                    removed && "line-through",
                    changed && "bg-blue-accent/15 text-blue-accent-foreground",
                    !locked && "cursor-text hover:ring-1 hover:ring-inset hover:ring-primary/40"
                  )}
                  style={widthOfField(field)}
                >
                  {text}
                </div>
              );
              const hint = locked ?? (text.length > 40 ? text : null);
              return hint ? <Tip content={hint}>{body}</Tip> : body;
            };
            return (
              <ContextMenu key={virtual.key}>
                <ContextMenuTrigger asChild>
                  <div
                    className={cn(
                      "absolute left-0 right-0 flex border-b border-border/30 text-[11px] hover:bg-muted/40",
                      removed && "opacity-50"
                    )}
                    style={style}
                  >
                    <div className="w-12 shrink-0 px-2 py-1 font-mono text-muted-foreground">
                      {row.index}
                    </div>
                    {cell("key")}
                    {cell("source")}
                    {hasTags && cell("tag")}
                  </div>
                </ContextMenuTrigger>
                <ContextMenuContent>
                  <ContextMenuItem
                    disabled={!!session.locked || removed}
                    onSelect={() => setEditing(`${key}key`)}
                  >
                    <Pencil size={14} />
                    Edit key
                  </ContextMenuItem>
                  <ContextMenuItem
                    disabled={!!session.locked || removed}
                    onSelect={() => setEditing(`${key}source`)}
                  >
                    <Pencil size={14} />
                    Edit source string
                  </ContextMenuItem>
                  <Tip
                    content="The marker string after the source. This game writes Encrypt on some lobby entries and nothing elsewhere."
                    side="right"
                  >
                    <ContextMenuItem
                      disabled={!!session.locked || removed}
                      onSelect={() => {
                        setTagsWanted(true);
                        setEditing(`${key}tag`);
                      }}
                    >
                      <Pencil size={14} />
                      Edit tag
                    </ContextMenuItem>
                  </Tip>
                  <ContextMenuItem
                    disabled={!!session.locked || removed}
                    onSelect={() => setMetaAsk({ index: row.index, key: row.key })}
                  >
                    <Pencil size={14} />
                    Edit metadata…{row.metadata.length > 0 ? ` (${row.metadata.length})` : ""}
                  </ContextMenuItem>
                  <ContextMenuSeparator />
                  <ContextMenuItem
                    disabled={!!session.locked || removed}
                    onSelect={() => session.setDraft(target, { op: "string_remove" }, [])}
                    className="text-destructive focus:text-destructive"
                  >
                    <Trash2 size={14} />
                    Remove entry
                  </ContextMenuItem>
                  <ContextMenuSeparator />
                  <ContextMenuItem disabled={!anyDraft} onSelect={() => dropEntryDrafts(row.index)}>
                    <Undo2 size={14} />
                    Discard changes
                  </ContextMenuItem>
                </ContextMenuContent>
              </ContextMenu>
            );
          })}
        </div>
      </div>

      <AlertDialog open={metaAsk !== null} onOpenChange={(open) => !open && setMetaAsk(null)}>
        <AlertDialogContent>
          {metaAsk &&
            (() => {
              const entry = table.entries[metaAsk.index];
              const rows: MetaRow[] = entry.metadata.map(([id, value]) => {
                const held = draftOf(metaAsk.index, metaAsk.key, `meta:${id}`)?.draft;
                return {
                  id,
                  held: value,
                  draft:
                    held?.op === "string_meta_set" || held?.op === "string_meta_remove"
                      ? held
                      : null,
                };
              });
              for (const record of draftsHere.values()) {
                const field = record.target.string?.field;
                if (
                  record.target.string?.index === metaAsk.index &&
                  record.draft.op === "string_meta_set" &&
                  field?.startsWith("meta:") &&
                  !entry.metadata.some(([id]) => `meta:${id}` === field)
                ) {
                  rows.push({ id: field.slice(5), held: null, draft: record.draft });
                }
              }
              const removed = draftOf(metaAsk.index, metaAsk.key)?.draft?.op === "string_remove";
              const locked = session.locked ?? (removed ? "This entry is being removed." : null);
              return (
                <MetaDataForm
                  entry={metaAsk.key}
                  rows={rows}
                  locked={locked}
                  onSet={(id, to) =>
                    session.setDraft(
                      stringTarget(exportIndex, metaAsk.index, metaAsk.key, `meta:${id}`),
                      { op: "string_meta_set", id, to },
                      []
                    )
                  }
                  onRemove={(id) =>
                    session.setDraft(
                      stringTarget(exportIndex, metaAsk.index, metaAsk.key, `meta:${id}`),
                      { op: "string_meta_remove", id },
                      []
                    )
                  }
                  onDiscard={(id) =>
                    session.dropDraft(
                      draftKey(stringTarget(exportIndex, metaAsk.index, metaAsk.key, `meta:${id}`))
                    )
                  }
                />
              );
            })()}
        </AlertDialogContent>
      </AlertDialog>

      <div className="flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-3 py-2 text-[11px]">
        <Tip
          content={
            addClash
              ? `This table already has an entry keyed ${addKeyTrimmed}.`
              : "The new entry's key, unique within the table."
          }
        >
          <Input
            value={newKey}
            onChange={(e) => setNewKey(e.target.value)}
            placeholder="Key"
            className={cn("h-7 w-48 font-mono text-[11px]", addClash && "ring-1 ring-destructive")}
          />
        </Tip>
        <Input
          value={newSource}
          onChange={(e) => setNewSource(e.target.value)}
          placeholder="Source string"
          className="h-7 min-w-0 flex-1 font-mono text-[11px]"
        />
        <Button
          size="sm"
          variant="outline"
          className="h-7"
          disabled={!canAdd}
          onClick={() => {
            session.setDraft(
              stringTarget(exportIndex, null, addKeyTrimmed),
              { op: "string_add", source: newSource },
              []
            );
            setNewKey("");
            setNewSource("");
          }}
        >
          <Plus size={13} /> Add entry
        </Button>
      </div>
    </div>
  );
}

function DataTableGrid({ table, exportIndex }: { table: DataTable; exportIndex: number }) {
  const session = useEditSession();
  const [filter, setFilter] = useState("");
  const [naming, setNaming] = useState<RowNaming | null>(null);
  const [keyAsk, setKeyAsk] = useState<KeyAsk | null>(null);
  // Which cell shows an input. Kept here rather than in the session because the row detail panel
  // can show the same field, and two inputs fighting over focus would commit and drop the draft.
  const [editing, setEditing] = useState<string | null>(null);
  const [widths, setWidths] = useState<Record<string, number>>({});
  // By name rather than by index: the filter re-orders the rows underneath the panel.
  const [openRow, setOpenRow] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  const rows = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return table.rows;
    return table.rows.filter(
      (row) =>
        row.name.toLowerCase().includes(needle) ||
        row.fields.some((f) => summarise(f.value).toLowerCase().includes(needle))
    );
  }, [filter, table.rows]);

  const rowDrafts = useMemo(() => {
    const byName = new Map<string, Draft>();
    for (const record of Object.values(session.drafts)) {
      if (record.target.row?.export === exportIndex) {
        byName.set(record.target.row.name, record.draft);
      }
    }
    return byName;
  }, [session.drafts, exportIndex]);
  const positionOf = useMemo(
    () => new Map(table.rows.map((row, index) => [row.name, index])),
    [table.rows]
  );
  // Names the table will hold once the drafts land, for refusing a duplicate before the backend
  // has to.
  const taken = useMemo(() => {
    const names = new Set(table.rows.map((row) => row.name.toLowerCase()));
    for (const [name, draft] of rowDrafts) {
      if (draft.op === "row_add" || draft.op === "row_duplicate") names.add(name.toLowerCase());
      if (draft.op === "row_rename") names.add(draft.to.toLowerCase());
    }
    return names;
  }, [table.rows, rowDrafts]);
  // Pending adds and copies have no bytes yet, so they show as ghost rows where they will land.
  // Under a filter the positions mean nothing among the matches, so they go last.
  const gridRows = useMemo<GridRow[]>(() => {
    const ghosts: { name: string; source?: string; at: number }[] = [];
    for (const [name, draft] of rowDrafts) {
      if (draft.op === "row_add") ghosts.push({ name, at: draft.at });
      else if (draft.op === "row_duplicate")
        ghosts.push({ name, source: draft.source, at: draft.at });
    }
    const ghost = (g: { name: string; source?: string }): GridRow => ({
      kind: "ghost",
      name: g.name,
      source: g.source,
    });
    const filtered = rows !== table.rows;
    const out: GridRow[] = [];
    for (const row of rows) {
      if (!filtered) {
        const at = positionOf.get(row.name);
        out.push(...ghosts.filter((g) => g.at === at).map(ghost));
      }
      out.push({ kind: "row", row });
    }
    out.push(...ghosts.filter((g) => filtered || g.at >= table.rows.length).map(ghost));
    return out;
  }, [rows, rowDrafts, positionOf, table.rows]);

  // Sized from the widest value actually present, so a column of booleans does not get the same
  // room as a column of asset paths. Sampled because a table can hold tens of thousands of rows.
  // `natural` keeps the uncapped width so the fill below can tell which columns are truly clipped.
  const { measured, natural } = useMemo(() => {
    const sample = table.rows.slice(0, 200);
    const measured: Record<string, number> = {};
    const natural: Record<string, number> = {};
    const record = (key: string, longest: number) => {
      natural[key] = longest * 7.1 + 26;
      measured[key] = autoWidth(longest);
    };
    // Pending names count too, so a ghost row or a rename is not cut short by the column.
    const drafted = [...rowDrafts].flatMap(([name, draft]) =>
      draft.op === "row_rename" ? [draft.to] : [name]
    );
    record(
      ROW_KEY,
      [...sample.map((r) => r.name), ...drafted].reduce((w, name) => Math.max(w, name.length), 5)
    );
    for (const column of table.columns) {
      let longest = displayName(column).length;
      for (const row of sample) {
        const field = row.fields.find((f) => labelOf(f) === column);
        if (field) longest = Math.max(longest, summarise(field.value).length);
      }
      record(column, longest);
    }
    return { measured, natural };
  }, [table.rows, table.columns, rowDrafts]);

  // Spare width is shared out so a narrow table does not sit in the corner of a wide window, but
  // each column is capped: a boolean should never end up as wide as an asset path.
  const [viewportWidth, setViewportWidth] = useState(0);
  useEffect(() => {
    const element = scrollRef.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => setViewportWidth(entry.contentRect.width));
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  const layout = useMemo(() => {
    const keys = [ROW_KEY, ...table.columns];
    const out: Record<string, number> = {};
    for (const key of keys) out[key] = widths[key] ?? measured[key] ?? 150;

    // Spare width goes first to the columns whose content is actually being cut off, and only
    // then to the rest. Sharing it out purely in proportion would hand the most room to whichever
    // column was already widest, which is rarely the one that needs it.
    const clipped = (key: string) => (natural[key] ?? 0) > MAX_AUTO_COLUMN;
    const cap = (key: string) => {
      const base = measured[key] ?? 150;
      return clipped(key)
        ? Math.min(natural[key] ?? base, MAX_CLIPPED_COLUMN)
        : Math.min(Math.max(base * 1.5, base + 60), MAX_FILLED_COLUMN);
    };
    // Columns the reader dragged keep exactly the width they were given.
    const auto = keys.filter((key) => widths[key] === undefined);
    const fill = (candidates: string[]) => {
      for (let pass = 0; pass < 3; pass++) {
        const total = keys.reduce((sum, key) => sum + out[key], 0);
        const slack = viewportWidth - total - 1;
        if (slack <= 1) return;
        const growable = candidates.filter((key) => out[key] < cap(key));
        if (growable.length === 0) return;
        const share = growable.reduce((sum, key) => sum + out[key], 0);
        for (const key of growable) {
          out[key] = Math.min(cap(key), out[key] + (slack * out[key]) / share);
        }
      }
    };
    fill(auto.filter(clipped));
    fill(auto);
    return out;
  }, [table.columns, widths, measured, natural, viewportWidth]);

  const widthOf = useCallback((key: string) => layout[key] ?? 150, [layout]);
  const totalWidth = useMemo(
    () => table.columns.reduce((sum, c) => sum + widthOf(c), widthOf(ROW_KEY)),
    [table.columns, widthOf]
  );

  const startResize = useCallback(
    (key: string, event: React.MouseEvent) => {
      event.preventDefault();
      event.stopPropagation();
      const startX = event.clientX;
      const startWidth = widthOf(key);
      const move = (e: MouseEvent) => {
        setWidths((w) => ({ ...w, [key]: Math.max(MIN_COLUMN, startWidth + e.clientX - startX) }));
      };
      const up = () => {
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
    },
    [widthOf]
  );

  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: gridRows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 26,
    overscan: 20,
  });

  const copyCsv = useCallback(() => {
    const escape = (cell: string) => (/[",\n]/.test(cell) ? `"${cell.replace(/"/g, '""')}"` : cell);
    const header = ["Row", ...table.columns].map(escape).join(",");
    const body = rows.map((row) => {
      const byName = new Map(row.fields.map((f) => [labelOf(f), summarise(f.value)]));
      return [row.name, ...table.columns.map((c) => byName.get(c) ?? "")].map(escape).join(",");
    });
    void navigator.clipboard.writeText([header, ...body].join("\n"));
  }, [rows, table.columns]);

  const detail = openRow === null ? null : rows.find((row) => row.name === openRow);
  const detailRows = useMemo(
    () => (detail ? detail.fields.map((field) => rowOf(field, [])) : []),
    [detail]
  );

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
        <Table2 size={13} className="shrink-0 text-muted-foreground" />
        <span className="min-w-0 truncate text-xs text-muted-foreground">
          {rows.length === table.rows.length
            ? `${table.rows.length} rows`
            : `${rows.length} of ${table.rows.length} rows`}{" "}
          of <span className="font-mono text-foreground/80">{table.row_struct}</span>
        </span>
        <Input
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter rows"
          className="ml-auto h-7 w-28 min-w-0 flex-1 text-xs sm:max-w-56"
        />
        <Button size="sm" variant="outline" className="h-7 shrink-0" onClick={copyCsv}>
          <Copy size={12} /> CSV
        </Button>
      </div>

      {table.truncated && (
        <div className="shrink-0 border-b border-amber-500/30 bg-amber-500/10 px-3 py-1.5 text-[11px] text-amber-300">
          Stopped after {table.rows.length} of {table.declared_rows} rows: {table.truncated}
        </div>
      )}

      <div className="flex min-h-0 min-w-0 flex-1">
        <div ref={scrollRef} className="min-h-0 min-w-0 flex-1 overflow-auto">
          <div style={{ width: totalWidth }}>
            <div className="sticky top-0 z-10 flex border-b border-border bg-card text-[10px] font-semibold uppercase text-muted-foreground">
              <HeaderCell
                label="Row"
                title="Row name"
                width={widthOf(ROW_KEY)}
                onResize={(e) => startResize(ROW_KEY, e)}
              />
              {table.columns.map((column) => (
                <HeaderCell
                  key={column}
                  label={displayName(column)}
                  title={column}
                  width={widthOf(column)}
                  onResize={(e) => startResize(column, e)}
                />
              ))}
            </div>
            <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
              {virtualizer.getVirtualItems().map((virtual) => {
                const item = gridRows[virtual.index];
                if (item.kind === "ghost") {
                  return (
                    <GhostRow
                      key={virtual.key}
                      name={item.name}
                      source={item.source}
                      nameWidth={widthOf(ROW_KEY)}
                      style={{ top: virtual.start, height: virtual.size, width: totalWidth }}
                      onDiscard={() =>
                        session.dropDraft(draftKey(rowTarget(exportIndex, item.name)))
                      }
                    />
                  );
                }
                const row = item.row;
                const rowKey = draftKey(rowTarget(exportIndex, row.name));
                const rowDraft = rowDrafts.get(row.name);
                const removed = rowDraft?.op === "row_remove";
                const byName = new Map(row.fields.map((f) => [labelOf(f), f]));
                return (
                  <div
                    key={virtual.key}
                    onClick={() => setOpenRow(row.name)}
                    className={cn(
                      "absolute left-0 flex cursor-pointer border-b border-border/30 text-[11px] hover:bg-muted/40",
                      openRow === row.name && "bg-muted/60",
                      removed && "opacity-50"
                    )}
                    style={{ top: virtual.start, height: virtual.size, width: totalWidth }}
                  >
                    <RowNameCell
                      row={row}
                      position={positionOf.get(row.name) ?? table.rows.length}
                      draft={rowDraft}
                      width={widthOf(ROW_KEY)}
                      locked={session.locked}
                      onAdd={(at) => setNaming({ kind: "add", at })}
                      onDuplicate={(at) => setNaming({ kind: "duplicate", source: row.name, at })}
                      onRename={() => setNaming({ kind: "rename", source: row.name })}
                      onRemove={() =>
                        session.setDraft(rowTarget(exportIndex, row.name), { op: "row_remove" }, [])
                      }
                      onDiscard={() => session.dropDraft(rowKey)}
                    />
                    {table.columns.map((column) => {
                      const field = byName.get(column);
                      const target = field ? entryTarget(field) : null;
                      const key = target ? draftKey(target) : null;
                      const draft = key ? session.drafts[key]?.draft : undefined;
                      const stored = field ? isStored(field) : false;
                      const count = field ? elementCount(field.value) : null;
                      const text =
                        draftText(draft, count) ??
                        (field
                          ? stored || field.value.kind === "unset"
                            ? summarise(field.value)
                            : "(default)"
                          : "");
                      const isUnset = field?.value.kind === "unset";
                      const storable =
                        field?.value.kind === "unset" &&
                        !VALUE_ONLY_DECLARED.has(field.value.declared);
                      const locked =
                        session.locked ??
                        (removed ? "This row is being removed." : editableReason(field));
                      // A change to the element count moves what the detail panel may have drafted
                      // inside this container, so the two exclude each other.
                      const held = key !== null && hasDraftsWithin(session, key);
                      const structural = draft !== undefined && isStructural(draft);
                      // Within the row, so removing the row discards what was drafted in it.
                      const queue = (next: Draft) => {
                        if (target) session.setDraft(target, next, [rowKey]);
                      };
                      const drop = () => {
                        if (key) session.dropDraft(key);
                      };
                      if (key !== null && editing === key && !locked) {
                        return (
                          <div
                            key={column}
                            className="shrink-0 px-0.5 py-0.5"
                            style={{ width: widthOf(column) }}
                          >
                            <ValueInput
                              value={field?.value ?? { kind: "default" }}
                              initial={
                                draft?.op === "set"
                                  ? draft.text
                                  : field && stored
                                    ? editText(field.value)
                                    : ""
                              }
                              onCommit={(next) => {
                                setEditing(null);
                                // Typing a value back to what the file already holds is not a
                                // change, so the draft is dropped rather than queued. A value
                                // holding its default has nothing to type back to.
                                const original = field && stored ? editText(field.value) : null;
                                if (next === original) drop();
                                else queue({ op: "set", text: next });
                              }}
                              onCancel={() => setEditing(null)}
                            />
                          </div>
                        );
                      }
                      const cell = (
                        <div
                          // Click, not double-click: opening the row panel re-lays out the
                          // columns, so the cell moves out from under the pointer and a second
                          // click would land somewhere else.
                          onClick={(e) => {
                            if (locked || key === null) return;
                            e.stopPropagation();
                            setEditing(key);
                          }}
                          className={cn(
                            "shrink-0 truncate px-2 py-1 font-mono",
                            !stored && draft === undefined && "text-muted-foreground/50 italic",
                            draft !== undefined && "bg-blue-accent/15 text-blue-accent-foreground",
                            !locked &&
                              "cursor-text hover:ring-1 hover:ring-inset hover:ring-primary/40"
                          )}
                          style={{ width: widthOf(column) }}
                        >
                          {text}
                        </div>
                      );
                      const trigger = <ContextMenuTrigger asChild>{cell}</ContextMenuTrigger>;
                      // The lock reason wins; otherwise a long value gets its full text, since the
                      // column is narrower than most strings.
                      const hint = locked && field ? locked : text.length > 24 ? text : null;
                      return (
                        <ContextMenu key={column}>
                          {hint ? <Tip content={hint}>{trigger}</Tip> : trigger}
                          <ContextMenuContent>
                            <ContextMenuItem
                              disabled={!!locked || key === null}
                              onSelect={() => setEditing(key)}
                            >
                              <Pencil size={14} />
                              Edit value
                            </ContextMenuItem>
                            {storable && (
                              <ContextMenuItem
                                disabled={!!session.locked}
                                onSelect={() => queue({ op: "store" })}
                              >
                                <Plus size={14} />
                                Store empty value
                              </ContextMenuItem>
                            )}
                            <ContextMenuItem
                              disabled={
                                !!session.locked ||
                                key === null ||
                                (!stored && !isUnset && draft?.op !== "set")
                              }
                              onSelect={() => queue({ op: "clear" })}
                            >
                              <Eraser size={14} />
                              Set to zero
                            </ContextMenuItem>
                            <ContextMenuItem
                              disabled={!!session.locked || key === null || isUnset}
                              onSelect={() => queue({ op: "unset" })}
                            >
                              <Undo2 size={14} />
                              Inherit default
                            </ContextMenuItem>
                            {count !== null && (
                              <>
                                <ContextMenuSeparator />
                                <ContextMenuItem
                                  // An array grows by a copy of the element already there; a set
                                  // or a map keys on its contents, so its new element is asked
                                  // for a key first.
                                  disabled={!!session.locked || held || structural}
                                  onSelect={() => {
                                    const kind = field?.value.kind;
                                    if (target && (kind === "set" || kind === "map")) {
                                      setKeyAsk({
                                        target,
                                        within: [rowKey],
                                        index: count,
                                        label: column,
                                        kind,
                                        structKey: field ? keysAreStructs(field.value) : false,
                                      });
                                    } else {
                                      queue({ op: "insert", index: count });
                                    }
                                  }}
                                >
                                  <Plus size={14} />
                                  {field?.value.kind === "array" ? "Add element" : "Add element…"}
                                </ContextMenuItem>
                                <ContextMenuItem
                                  disabled={!!session.locked || held || structural || count === 0}
                                  onSelect={() => queue({ op: "remove", index: count - 1 })}
                                >
                                  <Minus size={14} />
                                  Drop last element
                                </ContextMenuItem>
                              </>
                            )}
                            <ContextMenuSeparator />
                            <ContextMenuItem disabled={draft === undefined} onSelect={drop}>
                              <Undo2 size={14} />
                              Discard change
                            </ContextMenuItem>
                          </ContextMenuContent>
                        </ContextMenu>
                      );
                    })}
                  </div>
                );
              })}
            </div>
          </div>
        </div>

        {detail && (
          <div className="flex w-[400px] shrink-0 flex-col border-l border-border">
            <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
              <span className="min-w-0 truncate font-mono text-xs text-foreground">
                {detail.name}
              </span>
              <Button
                size="sm"
                variant="ghost"
                className="ml-auto h-7 shrink-0"
                onClick={() => setOpenRow(null)}
              >
                <X size={13} />
              </Button>
            </div>
            <PropertyTree rows={detailRows} />
          </div>
        )}
      </div>

      <KeyAskDialog ask={keyAsk} session={session} onClose={() => setKeyAsk(null)} />
      <AlertDialog open={naming !== null} onOpenChange={(open) => !open && setNaming(null)}>
        <AlertDialogContent>
          {naming && (
            <RowNameForm
              key={`${naming.kind}:${"source" in naming ? naming.source : ""}:${"at" in naming ? naming.at : ""}`}
              naming={naming}
              taken={taken}
              onConfirm={(name) => {
                if (naming.kind === "add") {
                  session.setDraft(
                    rowTarget(exportIndex, name),
                    { op: "row_add", at: naming.at },
                    []
                  );
                } else if (naming.kind === "duplicate") {
                  session.setDraft(
                    rowTarget(exportIndex, name),
                    { op: "row_duplicate", source: naming.source, at: naming.at },
                    []
                  );
                } else if (name !== naming.source) {
                  session.setDraft(
                    rowTarget(exportIndex, naming.source),
                    { op: "row_rename", to: name },
                    []
                  );
                }
                setNaming(null);
              }}
            />
          )}
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

export default function AssetInspector({
  gamePath,
  container,
  entry,
  gameRunning,
  isActive,
  onClose,
  onOpenSettings,
  onOpenCopy,
  initialTarget,
}: Props) {
  const [pkg, setPkg] = useState<ParsedPackage | null>(null);
  /// A removal or reset the user has asked about but not yet confirmed.
  const [ask, setAsk] = useState<StructuralAsk | null>(null);
  /// Progress of an import index build started from the removal dialog.
  const [indexing, setIndexing] = useState<{ current: number; total: number } | null>(null);
  /// Bumped after a save so the asset is read again from disk rather than shown from memory.
  const [epoch, setEpoch] = useState(0);
  /// What to run once the user confirms abandoning unsaved edits.
  const [confirmLeave, setConfirmLeave] = useState<(() => void) | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(true);
  const [selected, setSelected] = useState(0);
  /// A statement to land on when a script was opened by following a call; tied to the export it
  /// was meant for, so picking another export by hand does not inherit it.
  const [scriptFocus, setScriptFocus] = useState<{ index: number; offset: number } | null>(null);
  /// The package row of the outline is selected, so the main pane shows the package's tables.
  const [showPackage, setShowPackage] = useState(false);
  /// Whether slots an export declares but does not store are listed. Off by default: a class
  /// default object declares far more than it stores, and the stored values are what matter.
  const [showInherited, setShowInherited] = useState<boolean>(() => readFlag(INHERITED_KEY));
  const toggleInherited = (next: boolean) => {
    setShowInherited(next);
    writeFlag(INHERITED_KEY, next);
  };
  const [mappings, setMappings] = useState<MappingsStatus | null>(null);
  const [view, setView] = useState<ViewMode>("table");
  // The default export is picked when an asset is opened, not again when a save re-reads it:
  // that would pull the user off the export they were editing.
  const pickDefault = useRef(true);
  // Read once, on the first load: the inspector is remounted for every new target.
  const target = useRef(initialTarget);
  useEffect(() => {
    pickDefault.current = true;
  }, [container, entry]);

  useEffect(() => {
    let cancelled = false;
    setBusy(true);
    setError(null);
    setPkg(null);
    void (async () => {
      try {
        const result = await invoke<ParsedPackage>("inspect_asset", {
          gameRoot: gamePath,
          container,
          entry,
        });
        if (cancelled) return;
        setPkg(result);
        const wanted = target.current
          ? result.exports.findIndex((e) => e.index === target.current?.exportIndex)
          : -1;
        if (pickDefault.current && wanted >= 0) {
          pickDefault.current = false;
          const offset = target.current?.offset;
          setSelected(wanted);
          setShowPackage(false);
          if (result.exports[wanted].script) {
            setView("script");
            if (offset !== undefined) setScriptFocus({ index: wanted, offset });
          } else {
            setView("tree");
          }
        } else if (pickDefault.current) {
          pickDefault.current = false;
          // A lone export is what the user came for, so open it. Anything with subobjects starts
          // from the package overview, with the table (or the first export) ready behind it.
          const table = result.exports.findIndex((e) => e.data_table);
          setSelected(table >= 0 ? table : 0);
          setShowPackage(result.exports.length > 1);
        } else {
          setSelected((index) => Math.min(index, Math.max(result.exports.length - 1, 0)));
        }
      } catch (e) {
        if (!cancelled) {
          setError(String(e));
          void invoke<MappingsStatus>("get_mappings_status").then((s) => {
            if (!cancelled) setMappings(s);
          });
        }
      } finally {
        if (!cancelled) setBusy(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [gamePath, container, entry, epoch]);

  const fileName = entry.split("/").pop() ?? entry;
  const active = pkg?.exports[selected];
  const exportNames = useMemo(
    () => new Set((pkg?.exports ?? []).filter((e) => e.script).map((e) => e.object_name)),
    [pkg]
  );
  const openScript = (name: string, offset?: number) => {
    const at = pkg?.exports.findIndex((e) => e.object_name === name) ?? -1;
    if (at < 0) return;
    const go = () => {
      setSelected(at);
      setShowPackage(false);
      setView("script");
      setScriptFocus(offset === undefined ? null : { index: at, offset });
    };
    if (at === selected) go();
    else
      guarded(() => {
        discard();
        go();
      });
  };
  const needsMappings = mappings !== null && !mappings.loaded;

  const onSaved = useCallback(() => setEpoch((n) => n + 1), []);
  const edits = useAssetEdits({
    gamePath,
    container,
    entry,
    exportIndex: selected,
    exportPath: active?.path,
    epoch,
    locked: active ? lockedReasonOf(active) : "Nothing is loaded.",
    onSaved,
  });
  const { discard } = edits;
  useSaveHotkeys({
    // Tabs stay mounted while hidden, so an inspector off screen must not answer the hotkeys.
    dirty: edits.dirty && isActive,
    saving: edits.saving,
    onSave: edits.save,
    onDiscard: () => setConfirmLeave(() => discard),
  });
  const guarded = (action: () => void) => {
    if (edits.dirty) setConfirmLeave(() => action);
    else action();
  };
  // Payload and bulk bytes travel through files: a save dialog on the way out, an open dialog on
  // the way in, with the replacement queued as a draft until the save.
  const exportBytes = async (kind: "payload" | "bulk", index: number, suggested: string) => {
    const path = await saveDialog({
      defaultPath: suggested,
      filters: [{ name: "Binary", extensions: ["bin"] }],
    });
    if (!path) return;
    try {
      const written = await invoke<number>(kind === "payload" ? "export_payload" : "export_bulk", {
        gameRoot: gamePath,
        container,
        entry,
        ...(kind === "payload" ? { export: index } : { resource: index }),
        path,
      });
      edits.report(`Wrote ${formatBytes(written)} to ${path}`, "ok");
    } catch (e: unknown) {
      edits.report(String(e), "err");
    }
  };
  const replaceBytes = async (kind: "payload" | "bulk", index: number) => {
    const picked = await openDialog({ multiple: false, directory: false });
    const path = Array.isArray(picked) ? picked[0] : picked;
    if (!path) return;
    edits.session.setDraft(
      kind === "payload" ? payloadTarget(index) : bulkTarget(index),
      kind === "payload"
        ? { op: "payload_replace", file: path }
        : { op: "bulk_replace", file: path },
      []
    );
  };
  const bytesActions: BytesActions = {
    exportPayload: (index, name) => void exportBytes("payload", index, `${name}.payload.bin`),
    replacePayload: (index) => void replaceBytes("payload", index),
    exportBulk: (index) => void exportBytes("bulk", index, `${fileName}.bulk${index}.bin`),
    replaceBulk: (index) => void replaceBytes("bulk", index),
  };
  const askRemoval = (index: number) => {
    setAsk({ kind: "remove", index, plan: null, error: null });
    invoke<RemovalPlan>("plan_export_removal", {
      gameRoot: gamePath,
      container,
      entry,
      exports: [index],
    })
      .then((plan) =>
        setAsk((held) =>
          held?.kind === "remove" && held.index === index ? { ...held, plan } : held
        )
      )
      .catch((e: unknown) =>
        setAsk((held) =>
          held?.kind === "remove" && held.index === index ? { ...held, error: String(e) } : held
        )
      );
  };
  const askRenamePlan = useCallback(
    (index: number, name: string) => {
      setAsk((held) =>
        held?.kind === "rename" && held.index === index
          ? { ...held, name, plan: null, error: null }
          : held
      );
      invoke<ExportEditPlan>("plan_export_edits", {
        gameRoot: gamePath,
        container,
        entry,
        edits: [{ op: "rename", export: index, name }],
      })
        .then((plan) =>
          setAsk((held) =>
            held?.kind === "rename" && held.index === index && held.name === name
              ? { ...held, plan }
              : held
          )
        )
        .catch((e: unknown) =>
          setAsk((held) =>
            held?.kind === "rename" && held.index === index && held.name === name
              ? { ...held, error: String(e) }
              : held
          )
        );
    },
    [gamePath, container, entry]
  );
  const clipboard = useExportClipboard();
  const askPastePlan = useCallback(
    (outer: number | null, name: string) => {
      setAsk((held) =>
        held?.kind === "paste" ? { ...held, outer, name, plan: null, error: null } : held
      );
      const source = clipboard.held;
      if (!source) return;
      invoke<CopyPlan>("plan_export_copy", {
        gameRoot: gamePath,
        container,
        entry,
        fromContainer: source.container,
        fromEntry: source.entry,
        export: source.export,
        intoOuter: outer,
        name,
      })
        .then((plan) =>
          setAsk((held) =>
            held?.kind === "paste" && held.outer === outer && held.name === name
              ? { ...held, plan }
              : held
          )
        )
        .catch((e: unknown) =>
          setAsk((held) =>
            held?.kind === "paste" && held.outer === outer && held.name === name
              ? { ...held, error: String(e) }
              : held
          )
        );
    },
    [clipboard.held, gamePath, container, entry]
  );
  const paste = useCallback(
    (outer: number | null, name: string) => {
      const source = clipboard.held;
      if (!source) return;
      setAsk(null);
      invoke<{ outcome: string; message?: string; pak: string; warnings?: string[] }>(
        "save_export_copy",
        {
          gameRoot: gamePath,
          container,
          entry,
          fromContainer: source.container,
          fromEntry: source.entry,
          export: source.export,
          intoOuter: outer,
          name,
          modName: edits.modName.trim(),
          replace: false,
          layer: edits.onModCopy,
          target: edits.saveTarget,
        }
      )
        .then((result) => {
          if (result.outcome === "holds_copy") {
            edits.report(
              `${result.pak} already holds an edited copy of this asset. Open that copy and paste there.`,
              "err"
            );
            return;
          }
          edits.report(withWarnings(result.message ?? "Pasted", result.warnings), "ok");
        })
        .catch((e: unknown) => edits.report(String(e), "err"));
    },
    [clipboard.held, gamePath, container, entry, edits]
  );
  const askRetypePlan = useCallback(
    (index: number, target: number) => {
      setAsk((held) =>
        held?.kind === "retype" && held.index === index
          ? { ...held, class: target, plan: null, error: null }
          : held
      );
      invoke<ExportEditPlan>("plan_export_edits", {
        gameRoot: gamePath,
        container,
        entry,
        edits: [{ op: "set_class", export: index, class: target }],
        resets: [index],
      })
        .then((plan) =>
          setAsk((held) =>
            held?.kind === "retype" && held.index === index && held.class === target
              ? { ...held, plan }
              : held
          )
        )
        .catch((e: unknown) =>
          setAsk((held) =>
            held?.kind === "retype" && held.index === index && held.class === target
              ? { ...held, error: String(e) }
              : held
          )
        );
    },
    [gamePath, container, entry]
  );
  const askDependencyPlan = useCallback(
    (index: number, runs: DependencyRuns) => {
      setAsk((held) =>
        held?.kind === "deps" && held.index === index
          ? { ...held, runs, plan: null, error: null }
          : held
      );
      invoke<DependencyPlan>("plan_dependency_edits", {
        gameRoot: gamePath,
        container,
        entry,
        edits: [{ export: index, runs }],
      })
        .then((plan) =>
          setAsk((held) =>
            held?.kind === "deps" && held.index === index ? { ...held, plan } : held
          )
        )
        .catch((e: unknown) =>
          setAsk((held) =>
            held?.kind === "deps" && held.index === index ? { ...held, error: String(e) } : held
          )
        );
    },
    [gamePath, container, entry]
  );
  const askDropImport = (at: number) => {
    setAsk({ kind: "drop_import", import: at, plan: null, error: null });
    invoke<ImportRemovalPlan>("plan_import_removal", {
      gameRoot: gamePath,
      container,
      entry,
      imports: [at],
    })
      .then((plan) =>
        setAsk((held) =>
          held?.kind === "drop_import" && held.import === at ? { ...held, plan } : held
        )
      )
      .catch((e: unknown) =>
        setAsk((held) =>
          held?.kind === "drop_import" && held.import === at ? { ...held, error: String(e) } : held
        )
      );
  };
  const buildIndex = async (index: number) => {
    setIndexing({ current: 0, total: 0 });
    const unlisten = await listen<{ current: number; total: number }>(
      "import-index-progress",
      (event) => setIndexing(event.payload)
    );
    try {
      await invoke("build_import_index", { gameRoot: gamePath });
      askRemoval(index);
    } catch (e: unknown) {
      setAsk((held) =>
        held?.kind === "remove" && held.index === index ? { ...held, error: String(e) } : held
      );
    } finally {
      unlisten();
      setIndexing(null);
    }
  };
  const structuralLock = gameRunning
    ? "Close the game to change the asset."
    : edits.dirty
      ? "Save or discard your edits first."
      : null;

  // A DataTable reads best as a grid, but the rows are still just properties, so the tree and
  // JSON views work on the same data rather than being a separate code path.
  const treeRows = useMemo<TreeRow[]>(() => {
    if (!active) return [];
    const shown = (entries: PropertyEntry[]) =>
      showInherited ? entries : withoutInherited(entries);
    if (!active.data_table) return shown(active.properties).map((property) => rowOf(property, []));
    const rowStruct = active.data_table.row_struct;
    return active.data_table.rows.map((row) =>
      rowOf(
        {
          name: row.name,
          value: { kind: "struct" as const, name: rowStruct, fields: shown(row.fields) },
        },
        []
      )
    );
  }, [active, showInherited]);
  const inheritedCount = active
    ? active.properties.filter((p) => p.value.kind === "unset").length
    : 0;

  // A function stores no property values, so a tree of it could only ever be empty.
  const codeOnly = Boolean(active?.script) && (active?.properties.length ?? 0) === 0;
  // Only the views that can show something for this export are offered at all.
  const views: { id: ViewMode; label: string }[] = [
    { id: "table" as const, label: "Table", shown: Boolean(active?.data_table) },
    { id: "strings" as const, label: "Strings", shown: Boolean(active?.string_table) },
    { id: "script" as const, label: "Script", shown: Boolean(active?.script) },
    { id: "tree" as const, label: "Tree", shown: !codeOnly },
    { id: "json" as const, label: "JSON", shown: true },
    // The shaded bytes are for finding where a read went wrong, so only an export that did not
    // decode offers them; `asset hex` and `asset trace` cover the rest from the CLI.
    {
      id: "bytes" as const,
      label: "Bytes",
      shown: active?.status.state === "failed" || active?.status.state === "partial",
    },
  ].filter((option) => option.shown);
  // "table" is the resting choice: it stands for whatever the export is best read as.
  const restingView: ViewMode = active?.data_table
    ? "table"
    : active?.string_table
      ? "strings"
      : codeOnly
        ? "script"
        : "tree";
  const effectiveView: ViewMode =
    view !== "table" && views.some((option) => option.id === view) ? view : restingView;

  return (
    <EditSessionContext.Provider value={edits.session}>
      <div className="absolute inset-0 z-20 flex flex-col bg-background">
        <div className="flex items-center gap-2 border-b border-border px-3 py-2">
          <Button size="sm" variant="ghost" className="h-7" onClick={() => guarded(onClose)}>
            <ArrowLeft size={13} /> Back
          </Button>
          <Tip content={entry} side="bottom" align="start">
            <span className="min-w-0 flex-1 truncate font-mono text-xs text-foreground/80">
              {fileName}
            </span>
          </Tip>
          {pkg && !showPackage && (
            <div className="flex shrink-0 items-center gap-2">
              {active && (
                <StatusBadge
                  status={active.status}
                  note={active.note}
                  undecoded={active.undecoded}
                />
              )}
              {active && (
                <ExportActions
                  export={active}
                  lock={structuralLock}
                  duplicateLock={duplicateLockOf(active, pkg, structuralLock)}
                  onDuplicate={() => setAsk({ kind: "duplicate", index: active.index })}
                  payload={{
                    lock: payloadLockOf(active, pkg),
                    drafted:
                      edits.session.drafts[draftKey(payloadTarget(active.index))] !== undefined,
                    onExport: () => bytesActions.exportPayload(active.index, active.object_name),
                    onReplace: () => bytesActions.replacePayload(active.index),
                    onDiscard: () => edits.session.dropDraft(draftKey(payloadTarget(active.index))),
                  }}
                  onRemove={() => askRemoval(active.index)}
                  onReset={() => setAsk({ kind: "reset", index: active.index })}
                  onRename={() =>
                    setAsk({
                      kind: "rename",
                      index: active.index,
                      name: active.object_name,
                      plan: null,
                      error: null,
                    })
                  }
                  onFlags={() => setAsk({ kind: "flags", index: active.index, set: 0, clear: 0 })}
                  onRetype={() =>
                    setAsk({
                      kind: "retype",
                      index: active.index,
                      class: null,
                      plan: null,
                      error: null,
                    })
                  }
                  onDeps={() =>
                    setAsk({
                      kind: "deps",
                      index: active.index,
                      runs: pkg.dependencies?.[active.index] ?? {
                        serialize_before_serialize: [],
                        create_before_serialize: [],
                        serialize_before_create: [],
                        create_before_create: [],
                      },
                      plan: null,
                      error: null,
                    })
                  }
                />
              )}
              {(effectiveView === "tree" || effectiveView === "table") && (
                <Tip
                  content={
                    showInherited
                      ? "Hide the slots this export does not store"
                      : "Show the slots this export declares but does not store. They take the value the class inherits, and can be given one of their own."
                  }
                  side="bottom"
                >
                  <Button
                    size="sm"
                    variant={showInherited ? "outline" : "ghost"}
                    className="h-7 px-2 text-[10px] font-semibold uppercase"
                    onClick={() => toggleInherited(!showInherited)}
                  >
                    {showInherited ? <Eye size={13} /> : <EyeOff size={13} />} Inherited
                  </Button>
                </Tip>
              )}
              <div className="flex items-center gap-0.5 rounded-md bg-muted p-0.5">
                {views.map((option) => (
                  <button
                    key={option.id}
                    onClick={() => setView(option.id)}
                    className={cn(
                      "rounded px-2 py-0.5 text-[10px] font-semibold uppercase transition-colors",
                      effectiveView === option.id
                        ? "bg-background text-foreground"
                        : "text-muted-foreground hover:text-foreground"
                    )}
                  >
                    {option.label}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>

        {/* Outside the loaded branch on purpose: a save re-reads the asset, and the notice has to
            outlive that. */}
        {edits.modCopy && !edits.onModCopy && onOpenCopy && (
          <div className="flex shrink-0 items-center gap-2 border-b border-border bg-warn/10 px-3 py-1.5 text-[11px]">
            <AlertTriangle size={13} className="shrink-0 text-warn" />
            <span className="min-w-0 truncate">
              {previewContainerFilename(edits.modName, edits.saveTarget)} already holds an edited
              copy of this asset. Edits made here would start over from this one.
            </span>
            <Button
              size="sm"
              variant="outline"
              className="ml-auto h-6 shrink-0"
              onClick={() => edits.modCopy && onOpenCopy(edits.modCopy)}
            >
              Open the edited copy
            </Button>
          </div>
        )}

        {(edits.dirty || edits.notice) && (
          <EditBar
            edits={edits}
            gamePath={gamePath}
            gameRunning={gameRunning}
            onOpenCopy={onOpenCopy}
          />
        )}

        {busy && (
          <div className="flex flex-1 items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 size={15} className="animate-spin" /> Reading asset
          </div>
        )}

        {!busy && error && (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
            <AlertTriangle size={26} className="text-amber-400/70" />
            {needsMappings ? (
              <>
                <p className="max-w-lg text-sm text-foreground">
                  This asset needs a .usmap mappings file.
                </p>
                <p className="max-w-lg text-xs text-muted-foreground">
                  Marvel Rivals ships packages with unversioned properties, so their property names
                  and types are not stored in the asset itself. Assets saved out of the editor keep
                  that information and open without a mappings file.
                </p>
                <Button size="sm" variant="outline" onClick={onOpenSettings}>
                  Set the mappings file
                </Button>
              </>
            ) : (
              <p className="max-w-2xl break-words text-sm text-muted-foreground">{error}</p>
            )}
          </div>
        )}

        {!busy && pkg && (
          <div className="flex min-h-0 min-w-0 flex-1">
            <div className="w-[240px] shrink-0 overflow-y-auto border-r border-border">
              <Tip content={pkg.package_name} side="right">
                <button
                  onClick={() => setShowPackage(true)}
                  className={cn(
                    "flex w-full flex-col gap-0.5 border-b border-border px-3 py-2 text-left transition-colors",
                    showPackage ? "bg-muted" : "hover:bg-muted/50"
                  )}
                >
                  <span className="flex items-center gap-1.5">
                    <Package size={11} className="shrink-0 text-muted-foreground" />
                    <span className="truncate font-mono text-[11px] text-foreground">
                      {pkg.package_name.split("/").pop()}
                    </span>
                  </span>
                  <span className="truncate text-[10px] text-muted-foreground">
                    {pkg.names.length} names · {pkg.imports.length} imports · {pkg.exports.length}{" "}
                    exports
                  </span>
                </button>
              </Tip>
              {pkg.exports.map((exp, index) => (
                <ContextMenu key={exp.index}>
                  <ContextMenuTrigger asChild>
                    <button
                      onClick={() => {
                        if (index === selected) {
                          setShowPackage(false);
                          return;
                        }
                        guarded(() => {
                          discard();
                          setSelected(index);
                          setShowPackage(false);
                        });
                      }}
                      className={cn(
                        "flex w-full flex-col gap-0.5 border-b border-border/40 py-2 pr-3 pl-6 text-left transition-colors",
                        !showPackage && index === selected ? "bg-muted" : "hover:bg-muted/50"
                      )}
                    >
                      <span className="flex items-center gap-1.5">
                        <span className="truncate font-mono text-[11px] text-foreground">
                          {exp.object_name}
                        </span>
                        {exp.data_table && <Table2 size={11} className="shrink-0 text-sky-400" />}
                      </span>
                      <span className="flex items-center gap-1.5">
                        <span className="truncate text-[10px] text-muted-foreground">
                          {exp.class_name}
                        </span>
                        <StatusBadge
                          status={exp.status}
                          note={exp.note}
                          undecoded={exp.undecoded}
                        />
                      </span>
                    </button>
                  </ContextMenuTrigger>
                  <ContextMenuContent>
                    <ContextMenuItem
                      disabled={!!structuralLock}
                      onSelect={() => askRemoval(exp.index)}
                    >
                      <Trash2 size={14} />
                      Remove export…
                    </ContextMenuItem>
                    <ContextMenuItem
                      disabled={!!resetLockOf(exp, structuralLock)}
                      onSelect={() => setAsk({ kind: "reset", index: exp.index })}
                    >
                      <RotateCcw size={14} />
                      Reset to defaults…
                    </ContextMenuItem>
                  </ContextMenuContent>
                </ContextMenu>
              ))}
            </div>

            <div className="flex min-h-0 min-w-0 flex-1 flex-col">
              {pkg.schema_fixups && pkg.schema_fixups.length > 0 && (
                <div className="shrink-0 border-b border-amber-500/30 bg-amber-500/10 px-3 py-2 text-[11px] text-amber-300">
                  Your mappings file declares{" "}
                  {pkg.schema_fixups.map((f) => `${f.struct_name}.${f.property}`).join(", ")}, which
                  this build does not store. It was skipped so the rest of the asset reads
                  correctly.
                </div>
              )}

              {!showPackage && active?.status.state === "failed" && (
                <div className="shrink-0 border-b border-red-500/30 bg-red-500/10 px-3 py-2 text-[11px] text-red-300">
                  {active.status.reason}
                </div>
              )}

              {showPackage ? (
                <PackageView
                  pkg={pkg}
                  edits={edits}
                  lock={structuralLock}
                  bytes={bytesActions}
                  onSelectExport={(index) => {
                    if (index === selected) {
                      setShowPackage(false);
                      return;
                    }
                    guarded(() => {
                      discard();
                      setSelected(index);
                      setShowPackage(false);
                    });
                  }}
                  onRemove={askRemoval}
                  onReset={(index) => setAsk({ kind: "reset", index })}
                  onDuplicate={(index) => setAsk({ kind: "duplicate", index })}
                  onRename={(index) =>
                    setAsk({
                      kind: "rename",
                      index,
                      name: pkg.exports[index].object_name,
                      plan: null,
                      error: null,
                    })
                  }
                  onFlags={(index) => setAsk({ kind: "flags", index, set: 0, clear: 0 })}
                  onRetype={(index) =>
                    setAsk({ kind: "retype", index, class: null, plan: null, error: null })
                  }
                  onCopyOut={(index) => {
                    const exp = pkg.exports[index];
                    clipboard.copy({
                      container,
                      entry,
                      export: index,
                      name: exp.object_name,
                      className: exp.class_name,
                      path: exp.path,
                    });
                    edits.report(`${exp.object_name} is ready to paste into another package`, "ok");
                  }}
                  clipboard={clipboard.held}
                  onPaste={() => {
                    if (!clipboard.held) return;
                    setAsk({
                      kind: "paste",
                      held: clipboard.held,
                      outer: null,
                      name: clipboard.held.name,
                      plan: null,
                      error: null,
                    });
                  }}
                  onDeps={(index) =>
                    setAsk({
                      kind: "deps",
                      index,
                      runs: pkg.dependencies?.[index] ?? {
                        serialize_before_serialize: [],
                        create_before_serialize: [],
                        serialize_before_create: [],
                        create_before_create: [],
                      },
                      plan: null,
                      error: null,
                    })
                  }
                  onDropImport={askDropImport}
                />
              ) : active && effectiveView === "table" && active.data_table ? (
                <DataTableGrid table={active.data_table} exportIndex={active.index} />
              ) : active && effectiveView === "strings" && active.string_table ? (
                <StringTableView table={active.string_table} exportIndex={active.index} />
              ) : active && effectiveView === "json" ? (
                <JsonView export={active} />
              ) : active && effectiveView === "bytes" ? (
                <BytesPane
                  key={epoch}
                  gamePath={gamePath}
                  container={container}
                  entry={entry}
                  exportIndex={active.index}
                />
              ) : active && effectiveView === "script" ? (
                <ScriptPane
                  key={`${epoch}:${active.index}:${scriptFocus?.offset ?? ""}`}
                  gamePath={gamePath}
                  container={container}
                  entry={entry}
                  exportIndex={active.index}
                  exportNames={exportNames}
                  focus={scriptFocus?.index === selected ? scriptFocus.offset : null}
                  onOpen={openScript}
                />
              ) : active && treeRows.length > 0 ? (
                <PropertyTree rows={treeRows} />
              ) : (
                <div className="min-h-0 min-w-0 flex-1 overflow-auto">
                  <p className="p-6 text-center text-sm text-muted-foreground">
                    {active && inheritedCount > 0 ? (
                      <>
                        Stores no properties. {inheritedCount} declared{" "}
                        {inheritedCount === 1
                          ? "property inherits its value"
                          : "properties inherit their values"}
                        .{" "}
                        <button
                          className="underline hover:text-foreground"
                          onClick={() => toggleInherited(true)}
                        >
                          Show inherited
                        </button>
                      </>
                    ) : active &&
                      pkg.unversioned_properties &&
                      active.properties.length === 0 &&
                      active.status.state !== "failed" ? (
                      "This class declares no properties of its own."
                    ) : (
                      "This export stores no properties."
                    )}
                  </p>
                </div>
              )}

              {!showPackage &&
                active?.trailing_hex &&
                active.status.state !== "payload" &&
                effectiveView !== "bytes" && (
                  <details className="shrink-0 border-t border-border">
                    <summary className="cursor-pointer px-3 py-1.5 text-[11px] text-muted-foreground hover:text-foreground">
                      Undecoded bytes
                    </summary>
                    <pre className="max-h-40 overflow-auto px-3 pb-3 font-mono text-[10px] leading-relaxed text-muted-foreground">
                      {active.trailing_hex}
                    </pre>
                  </details>
                )}
            </div>
          </div>
        )}

        <AlertDialog
          open={edits.pendingReplace !== null}
          onOpenChange={(open) => !open && edits.cancelReplace()}
        >
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>
                {edits.pendingReplace?.pak} already holds an edited copy
              </AlertDialogTitle>
              <AlertDialogDescription>
                {`To build on the edits saved there, open that copy and make ${
                  edits.count === 1 ? "this change" : "these changes"
                } in it; unsaved changes here point into this copy's bytes and do not carry over. Starting over writes ${fileName} as it is here plus ${
                  edits.pendingReplace?.structural || edits.count === 1
                    ? "this change"
                    : `these ${edits.count} changes`
                }, and the edits saved there earlier are lost.`}
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Keep it</AlertDialogCancel>
              {edits.modCopy && onOpenCopy && (
                <AlertDialogAction
                  onClick={() => {
                    const copy = edits.modCopy;
                    edits.cancelReplace();
                    if (copy) onOpenCopy(copy);
                  }}
                >
                  Open the edited copy
                </AlertDialogAction>
              )}
              <AlertDialogAction
                onClick={() => void edits.save({ replace: true })}
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                Start over
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>

        <AlertDialog
          open={edits.pendingDrift !== null}
          onOpenChange={(open) => !open && edits.cancelDrift()}
        >
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>
                {fileName} has changed since these edits were made
              </AlertDialogTitle>
              <AlertDialogDescription>
                {edits.pendingDrift?.message} Saving anyway writes each edit where it now lands.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction
                onClick={() => void edits.save({ allowDrift: true })}
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                Save anyway
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>

        {pkg && (
          <StructuralDialog
            ask={ask}
            pkg={pkg}
            modName={edits.modName}
            saveTarget={edits.saveTarget}
            saving={edits.saving}
            indexing={indexing}
            onBuildIndex={(index) => void buildIndex(index)}
            onRenamePlan={askRenamePlan}
            onRetypePlan={askRetypePlan}
            onDependencyPlan={askDependencyPlan}
            onPastePlan={askPastePlan}
            onPaste={paste}
            onClose={() => setAsk(null)}
            onConfirm={(structural) => {
              setAsk(null);
              void edits.save({ structural });
            }}
          />
        )}

        <AlertDialog
          open={confirmLeave !== null}
          onOpenChange={(open) => !open && setConfirmLeave(null)}
        >
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Discard unsaved changes?</AlertDialogTitle>
              <AlertDialogDescription>
                Edits that have not been saved as a mod will be lost.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Keep editing</AlertDialogCancel>
              <AlertDialogAction
                onClick={() => {
                  const leave = confirmLeave;
                  setConfirmLeave(null);
                  leave?.();
                }}
                className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              >
                Discard
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </EditSessionContext.Provider>
  );
}
