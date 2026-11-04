import { useEffect, useMemo, useState } from "react";

import { invoke } from "@tauri-apps/api/core";
import { AlertTriangle, Copy, Cpu, Palette } from "lucide-react";

import type { PropertyEntry, PropertyValue } from "@/components/AssetInspector";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Tip } from "@/components/ui/tooltip";
import {
  draftKey,
  elementTarget,
  entryTarget,
  useEditSession,
  type EditSession,
  type EditTarget,
} from "@/hooks/useAssetEdits";
import {
  cssColor,
  cssGradient,
  objectName,
  recolorable,
  rendererKey,
  rgbOf,
  shortFloat,
  sourceKey,
  sourceLabel,
  type ColorReport,
  type ColorSource,
  type Recolor,
  type RecolorPick,
  type RendererColors,
  type Rgba,
} from "@/lib/niagaraColors";
import { editCommand, type VmLeaf } from "@/lib/niagaraVm";
import { cn } from "@/lib/utils";

const CHECKER =
  "repeating-conic-gradient(rgb(120 120 120 / 0.35) 0% 25%, transparent 0% 50%) 50% / 10px 10px";

/** A strip of colours over a checkerboard, so alpha shows. */
function Strip({ colors, peak, className }: { colors: Rgba[]; peak: number; className?: string }) {
  return (
    <div
      className={cn("rounded-sm border border-border/60", className)}
      style={{ background: CHECKER }}
    >
      <div className="h-full w-full rounded-sm" style={{ background: cssGradient(colors, peak) }} />
    </div>
  );
}

function Chip({ color, peak }: { color: Rgba; peak: number }) {
  return (
    <span
      className="inline-block size-4 shrink-0 rounded-sm border border-border/60"
      style={{
        background: `linear-gradient(${cssColor(color, peak)}, ${cssColor(color, peak)}), ${CHECKER}`,
      }}
    />
  );
}

/** The tint, paint and hue tools, handing the change they make to `onApply`. */
function Tools({
  label,
  blocked,
  onApply,
}: {
  label: string;
  blocked: string | null;
  onApply: (tool: Recolor) => void;
}) {
  const [mode, setMode] = useState<Recolor["tool"]>("paint");
  const [hex, setHex] = useState("#9933ff");
  const [strength, setStrength] = useState("1");
  const [degrees, setDegrees] = useState("120");
  const tool = (): Recolor | null => {
    if (mode === "hue") {
      const turn = Number(degrees);
      return Number.isFinite(turn) ? { tool: "hue", degrees: turn } : null;
    }
    const rgb = rgbOf(hex);
    if (mode === "paint") return { tool: "paint", rgb };
    const scale = Number(strength);
    if (!Number.isFinite(scale) || scale < 0) return null;
    return { tool: "tint", rgb: [rgb[0] * scale, rgb[1] * scale, rgb[2] * scale] };
  };
  const hints: Record<Recolor["tool"], string> = {
    paint:
      "Gives every colour this hue at its own brightness, so fades and HDR glows keep their shape.",
    tint: "Multiplies every colour by this one, times the strength: past 1 brightens.",
    hue: "Turns every colour's hue by this many degrees. Greys stay grey.",
  };
  return (
    <div className="flex flex-wrap items-center gap-2 text-[11px]">
      {(["paint", "tint", "hue"] as const).map((option) => (
        <Tip key={option} content={hints[option]}>
          <Button
            size="sm"
            variant={mode === option ? "secondary" : "ghost"}
            className="h-6 px-2 text-[11px] capitalize"
            aria-pressed={mode === option}
            onClick={() => setMode(option)}
          >
            {option}
          </Button>
        </Tip>
      ))}
      {mode === "hue" ? (
        <Input
          aria-label="Degrees"
          value={degrees}
          onChange={(event) => setDegrees(event.target.value)}
          className="h-6 w-16 text-[11px]"
        />
      ) : (
        <input
          type="color"
          aria-label="Colour"
          value={hex}
          onChange={(event) => setHex(event.target.value)}
          className="h-6 w-8 cursor-pointer rounded border border-border bg-transparent"
        />
      )}
      {mode === "tint" && (
        <Input
          aria-label="Strength"
          value={strength}
          onChange={(event) => setStrength(event.target.value)}
          className="h-6 w-14 text-[11px]"
        />
      )}
      <Tip content={blocked}>
        <span>
          <Button
            size="sm"
            className="h-6 px-2 text-[11px]"
            disabled={blocked !== null || tool() === null}
            onClick={() => {
              const chosen = tool();
              if (chosen) onApply(chosen);
            }}
          >
            {label}
          </Button>
        </span>
      </Tip>
    </div>
  );
}

/** An entry along a dotted path of struct fields, with the draft keys of the structs above it. */
function entryAt(
  properties: PropertyEntry[],
  path: string
): { entry: PropertyEntry; within: string[] } | null {
  if (path.includes("[")) return null;
  let fields = properties;
  const within: string[] = [];
  const names = path.split(".");
  for (const [at, name] of names.entries()) {
    const entry = fields.find((field) => field.name === name);
    if (!entry) return null;
    if (at === names.length - 1) return { entry, within };
    const target = entryTarget(entry);
    if (!target || entry.value.kind !== "struct") return null;
    within.push(draftKey(target));
    fields = entry.value.fields;
  }
  return null;
}

function numberOf(value: PropertyValue | undefined): number {
  return value?.kind === "float" || value?.kind === "byte" || value?.kind === "int"
    ? value.value
    : 0;
}

/** Why a container cannot take a whole-list draft: the export is locked, or a draft already sits
 *  inside it or on it. */
function listLock(
  session: EditSession,
  target: EditTarget | null,
  within: string[]
): string | null {
  if (session.locked) return session.locked;
  if (!target) return "This value has no recorded position in the file.";
  const key = draftKey(target);
  const above = within.find((outer) => session.drafts[outer]);
  if (above) return `Finish or discard the change on ${session.drafts[above].target.name} first.`;
  if (Object.values(session.drafts).some((record) => record.within.includes(key)))
    return `Save or discard the element edits on ${target.name} first.`;
  const held = session.drafts[key]?.draft;
  if (held && held.op !== "set_items") return `Save or discard the change to ${target.name} first.`;
  return null;
}

/** A list of numbers held as one `set_items` draft: what it reads with the draft applied, and a
 *  writer that keeps the read text where a number is unchanged, so only changed elements save. */
function useList(entry: PropertyEntry | null, within: string[]) {
  const session = useEditSession();
  const target = entry ? entryTarget(entry) : null;
  const items = entry?.value.kind === "array" ? entry.value.items : [];
  const heldText = items.map((item) =>
    item.kind === "float" || item.kind === "byte" ? String(item.value) : "0"
  );
  const heldValues = items.map(numberOf);
  const draft = target ? session.drafts[draftKey(target)]?.draft : undefined;
  const values = draft?.op === "set_items" ? draft.items.map(Number) : heldValues;
  const bytes = entry?.value.kind === "array" && items[0]?.kind === "byte";
  const write = (next: number[]) => {
    if (!target) return;
    const texts = next.map((value, at) => {
      const same = bytes
        ? value === heldValues[at]
        : Math.fround(value) === Math.fround(heldValues[at]);
      return same ? heldText[at] : String(bytes ? value : Math.fround(value));
    });
    if (texts.every((text, at) => text === heldText[at])) session.dropDraft(draftKey(target));
    else session.setDraft(target, { op: "set_items", items: texts, held: heldText }, within);
  };
  return { values, write, lock: listLock(session, target, within), changed: draft !== undefined };
}

function toBytes(color: Rgba): number[] {
  const view = new DataView(new ArrayBuffer(16));
  color.forEach((channel, at) => view.setFloat32(at * 4, channel, true));
  return Array.from(new Uint8Array(view.buffer));
}

function fromBytes(bytes: number[]): Rgba {
  const view = new DataView(new Uint8Array(bytes.slice(0, 16)).buffer);
  return [0, 4, 8, 12].map((at) =>
    bytes.length >= at + 4 ? view.getFloat32(at, true) : 0
  ) as Rgba;
}

/** Four number boxes for one colour, each committed on blur or Enter. */
function ChannelInputs({
  color,
  label,
  disabled,
  onCommit,
}: {
  color: Rgba;
  label: string;
  disabled: boolean;
  onCommit: (next: Rgba) => void;
}) {
  return (
    <>
      {color.map((channel, at) => (
        <Input
          // Remounted when the value changes elsewhere, so a tool's result shows.
          key={`${at}:${channel}`}
          aria-label={`${label} ${"RGBA"[at]}`}
          defaultValue={shortFloat(channel)}
          disabled={disabled}
          className="h-6 w-16 px-1 text-[11px] tabular-nums"
          onBlur={(event) => {
            const typed = Number(event.target.value);
            if (!Number.isFinite(typed) || shortFloat(typed) === shortFloat(channel)) return;
            const next = [...color] as Rgba;
            next[at] = typed;
            onCommit(next);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
          }}
        />
      ))}
    </>
  );
}

async function recolored(colors: Rgba[], tool: Recolor): Promise<Rgba[]> {
  return invoke<Rgba[]>("recolor_colors", { colors, tool });
}

function LutEditor({
  source,
  properties,
}: {
  source: Extract<ColorSource, { kind: "lut" }>;
  properties: PropertyEntry[];
}) {
  const entry = properties.find((property) => property.name === "ShaderLUT") ?? null;
  const list = useList(entry, []);
  const samples: Rgba[] = [];
  for (let at = 0; at + 3 < list.values.length; at += 4) {
    samples.push(list.values.slice(at, at + 4) as Rgba);
  }
  const peak = Math.max(1, ...samples.flatMap((sample) => sample.slice(0, 3)));
  const last = Math.max(samples.length - 1, 1);
  const timeAt = (at: number) =>
    source.min_time + ((source.max_time - source.min_time) * at) / last;
  const put = (next: Rgba[]) => list.write(next.flat());
  return (
    <div className="space-y-2">
      <Strip colors={samples} peak={peak} className="h-8" />
      <p className="text-[11px] text-muted-foreground">
        {samples.length} samples over {shortFloat(source.min_time)} to {shortFloat(source.max_time)}
        {peak > 1 ? `, HDR up to ${shortFloat(peak)} (shown scaled down)` : ""}
        {list.changed ? ", changed" : ""}
      </p>
      {!source.use_lut && (
        <p className="flex items-center gap-1 text-[11px] text-amber-400">
          <AlertTriangle size={11} /> This curve is read from its {source.keys} keys, not these
          samples.
        </p>
      )}
      {source.exposed_texture && (
        <Tip content={source.exposed_texture}>
          <p className="text-[11px] text-muted-foreground">
            Also kept as a texture, which a material reads only where a renderer binds it. An edit
            leaves the texture as it was.
          </p>
        </Tip>
      )}
      <Tools
        label="Apply to this curve"
        blocked={list.lock}
        onApply={(tool) => void recolored(samples, tool).then(put, () => undefined)}
      />
      <div className="max-h-80 overflow-auto rounded border border-border/60">
        <table className="w-full text-[11px]">
          <thead className="sticky top-0 bg-card text-left text-[10px] uppercase text-muted-foreground">
            <tr>
              <th className="px-2 py-1">#</th>
              <th className="px-2 py-1">Time</th>
              <th className="px-2 py-1" />
              <th className="px-2 py-1" colSpan={4}>
                R G B A
              </th>
            </tr>
          </thead>
          <tbody>
            {samples.map((sample, at) => (
              <tr key={at} className="border-t border-border/40">
                <td className="px-2 py-0.5 tabular-nums text-muted-foreground">{at}</td>
                <td className="px-2 py-0.5 tabular-nums">{shortFloat(timeAt(at))}</td>
                <td className="px-2 py-0.5">
                  <Chip color={sample} peak={peak} />
                </td>
                <td className="flex gap-1 px-2 py-0.5">
                  <ChannelInputs
                    color={sample}
                    label={`Sample ${at}`}
                    disabled={list.lock !== null}
                    onCommit={(next) => put(samples.map((was, i) => (i === at ? next : was)))}
                  />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function ArrayEditor({
  source,
  properties,
}: {
  source: Extract<ColorSource, { kind: "array" }>;
  properties: PropertyEntry[];
}) {
  const session = useEditSession();
  const entry = properties.find((property) => property.name === source.field);
  const container = entry ? entryTarget(entry) : null;
  const items = entry?.value.kind === "array" ? entry.value.items : [];
  const fieldOf = (at: number, channel: number) => {
    const item = items[at];
    return item?.kind === "struct"
      ? item.fields.find((field) => field.name === "RGBA"[channel])
      : undefined;
  };
  const shown = (at: number, channel: number): number => {
    const field = fieldOf(at, channel);
    const target = field ? entryTarget(field) : null;
    const draft = target ? session.drafts[draftKey(target)]?.draft : undefined;
    return draft?.op === "set" ? Number(draft.text) : numberOf(field?.value);
  };
  const colors: Rgba[] = items.map((_, at) => [0, 1, 2, 3].map((c) => shown(at, c)) as Rgba);
  const lock =
    session.locked ?? (container ? null : "This value has no recorded position in the file.");
  const put = (next: Rgba[]) => {
    if (!entry || !container) return;
    next.forEach((color, at) => {
      const element = elementTarget(entry, at);
      if (!element) return;
      const within = [draftKey(container), draftKey(element)];
      color.forEach((value, channel) => {
        const field = fieldOf(at, channel);
        const target = field ? entryTarget(field) : null;
        if (!field || !target) return;
        if (Math.fround(value) === Math.fround(numberOf(field.value)))
          session.dropDraft(draftKey(target));
        else session.setDraft(target, { op: "set", text: String(Math.fround(value)) }, within);
      });
    });
  };
  const peak = Math.max(1, ...colors.flatMap((color) => color.slice(0, 3)));
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap gap-1">
        {colors.map((color, at) => (
          <Chip key={at} color={color} peak={peak} />
        ))}
      </div>
      <Tools
        label="Apply to this array"
        blocked={lock}
        onApply={(tool) => void recolored(colors, tool).then(put, () => undefined)}
      />
      <div className="space-y-1">
        {colors.map((color, at) => (
          <div key={at} className="flex items-center gap-1 text-[11px]">
            <span className="w-8 tabular-nums text-muted-foreground">[{at}]</span>
            <Chip color={color} peak={peak} />
            <ChannelInputs
              color={color}
              label={`Element ${at}`}
              disabled={lock !== null}
              onCommit={(next) => put(colors.map((was, i) => (i === at ? next : was)))}
            />
          </div>
        ))}
      </div>
    </div>
  );
}

/** One copy of a parameter in a store, as a list of bytes to write the colour into. */
function useCopy(properties: PropertyEntry[], store: string) {
  const found = entryAt(properties, store);
  const data =
    found && found.entry.value.kind === "struct"
      ? (found.entry.value.fields.find((field) => field.name === "ParameterData") ?? null)
      : null;
  const storeTarget = found ? entryTarget(found.entry) : null;
  const within = found && storeTarget ? [...found.within, draftKey(storeTarget)] : [];
  return { list: useList(data, within), found: data !== null };
}

function ParameterEditor({
  source,
  properties,
}: {
  source: Extract<ColorSource, { kind: "parameter" }>;
  properties: PropertyEntry[];
}) {
  // Hooks run in a fixed order: a parameter has one or two copies, read in the order listed.
  const first = useCopy(properties, source.copies[0]?.store ?? "");
  const second = useCopy(properties, source.copies[1]?.store ?? "");
  const copies = [first, second].slice(0, source.copies.length);
  const offset = source.copies[0]?.offsets[0] ?? 0;
  const color = fromBytes(first.list.values.slice(offset, offset + 16));
  const missing = copies.some((copy) => !copy.found) || source.copies.length > 2;
  const lock = missing
    ? "This parameter's store sits where only the CLI's asset recolor reaches."
    : (copies.map((copy) => copy.list.lock).find((why) => why !== null) ?? null);
  const put = (next: Rgba) => {
    const bytes = toBytes(next);
    copies.forEach((copy, at) => {
      const values = [...copy.list.values];
      for (const start of source.copies[at].offsets) {
        bytes.forEach((byte, k) => {
          values[start + k] = byte;
        });
      }
      copy.list.write(values);
    });
  };
  const peak = Math.max(1, ...color.slice(0, 3));
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2 text-[11px]">
        <Chip color={color} peak={peak} />
        <span className="font-mono">{source.name}</span>
        <span className="text-muted-foreground">
          in {source.copies.map((copy) => copy.store).join(", ")}
        </span>
      </div>
      <Tools
        label="Apply to this parameter"
        blocked={lock}
        onApply={(tool) =>
          void recolored([color], tool).then(
            ([next]) => put(next),
            () => undefined
          )
        }
      />
      <div className="flex items-center gap-1">
        <ChannelInputs color={color} label={source.name} disabled={lock !== null} onCommit={put} />
      </div>
    </div>
  );
}

function ScriptView({
  source,
  entry,
  onOpenVm,
}: {
  source: Extract<ColorSource, { kind: "script" }>;
  entry: string;
  onOpenVm: () => void;
}) {
  return (
    <div className="space-y-2 text-[11px]">
      <div className="flex items-center gap-2">
        <span className="text-muted-foreground">{source.usage}</span>
        <Button
          size="sm"
          variant="outline"
          className="ml-auto h-6 px-2 text-[11px]"
          onClick={onOpenVm}
        >
          <Cpu size={11} /> Open in the VM view
        </Button>
      </div>
      <p className="text-muted-foreground">
        {source.drawn
          ? "A script's colours are changed from the CLI: asset recolor --scripts transforms each one a renderer draws where it is written, and asset edit-vm changes one constant, which a channel written from a literal copies."
          : "No renderer draws this colour: the ones drawn are worked out from it. asset edit-vm changes one of its constants, which a channel written from a literal copies."}
      </p>
      {source.channels.map((channel) => {
        const literals = channel.baked
          ? channel.leaves.filter(
              (leaf): leaf is Extract<VmLeaf, { kind: "constant" }> => leaf.kind === "constant"
            )
          : [];
        return (
          <div key={channel.at} className="flex items-center gap-2 border-b border-border/40 py-1">
            <span className="w-4 font-semibold">{channel.channel}</span>
            <Tip content={channel.expression}>
              <span className="min-w-0 flex-1 truncate font-mono">
                {channel.value !== undefined ? shortFloat(channel.value) : channel.expression}
              </span>
            </Tip>
            <span className="text-muted-foreground">{channel.baked ? "baked" : "computed"}</span>
            {literals.map((leaf) => (
              <Tip
                key={`${leaf.at}:${leaf.operand}`}
                content="Copy the command that gives this channel a new value"
              >
                <button
                  type="button"
                  aria-label={`Copy edit-vm for ${channel.channel}`}
                  className="text-muted-foreground hover:text-foreground"
                  onClick={() =>
                    void navigator.clipboard.writeText(editCommand(entry, source.object_name, leaf))
                  }
                >
                  <Copy size={11} />
                </button>
              </Tip>
            ))}
          </div>
        );
      })}
    </div>
  );
}

/** A material colour opaque, since a material's own alpha is rarely its opacity. */
function opaque([r, g, b]: Rgba): Rgba {
  return [r, g, b, 1];
}

function peakOf([r, g, b]: Rgba): number {
  return Math.max(1, r, g, b);
}

function MaterialView({ renderer }: { renderer: RendererColors }) {
  const many = renderer.materials.length !== 1;
  return (
    <div className="space-y-2 text-[11px]">
      <p className="text-muted-foreground">
        The colours {renderer.object_name} draws its material{many ? "s" : ""} with. Recolour effect
        with the material colours sets each on this renderer, which the engine applies over the
        material, so other effects drawing with it keep theirs.
      </p>
      {renderer.colors.map((color) => (
        <div
          key={`${color.material ?? ""}:${color.name}`}
          className="flex items-center gap-2 border-b border-border/40 py-1"
        >
          <Chip color={opaque(color.color)} peak={peakOf(color.color)} />
          <span className="min-w-0 flex-1 truncate">{color.name}</span>
          <span className="font-mono text-muted-foreground">
            {color.color.map(shortFloat).join(", ")}
          </span>
          <Tip content={color.from ?? "Set on the renderer, over every material's own"}>
            <span className="w-40 truncate text-muted-foreground">
              {color.from ? objectName(color.from) : "this renderer"}
            </span>
          </Tip>
          {color.enemy && <span className="text-red-400">enemy</span>}
          {color.bound && (
            <Tip content={`Set every frame from ${color.bound}, where a script writes it`}>
              <span className="text-muted-foreground">from {color.bound}</span>
            </Tip>
          )}
          {color.layered && <span className="text-muted-foreground">layer</span>}
        </div>
      ))}
      {renderer.unread?.map((why) => (
        <p key={why} className="flex items-start gap-1 text-warn">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" /> {why}
        </p>
      ))}
    </div>
  );
}

function Preview({ source }: { source: ColorSource }) {
  const peak = Math.max(1, source.peak ?? 1);
  switch (source.kind) {
    case "lut":
      return <Strip colors={source.samples} peak={peak} className="h-3 w-16 shrink-0" />;
    case "array":
      return <Strip colors={source.colors} peak={peak} className="h-3 w-16 shrink-0" />;
    case "parameter":
      return <Chip color={source.color} peak={peak} />;
    default:
      return <Cpu size={12} className="shrink-0 text-muted-foreground" />;
  }
}

/**
 * Where a Niagara package's colours come from, by emitter: each colour curve's baked samples,
 * colour arrays, colour parameters, the colours its CPU scripts write and those its renderers'
 * materials draw with. Curves, arrays and parameters are edited through the inspector's drafts,
 * one export at a time; Recolour effect changes every one in a save of its own.
 */
export function ColourPane({
  gamePath,
  container,
  entry,
  activeExport,
  properties,
  chosen,
  onChoose,
  onOpenVm,
  recolorBlocked,
  onRecolorAll,
}: {
  gamePath: string;
  container: string;
  entry: string;
  activeExport: number;
  properties: PropertyEntry[];
  /** The source last chosen, by its key, so a save's re-read lands on it again. */
  chosen: string | null;
  /** Picks a source or a renderer, by its export and the key it is chosen by. */
  onChoose: (exportIndex: number, key: string) => void;
  onOpenVm: (exportIndex: number) => void;
  recolorBlocked: string | null;
  onRecolorAll: (pick: RecolorPick, tool: Recolor) => void;
}) {
  const [report, setReport] = useState<ColorReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [enemyOnly, setEnemyOnly] = useState(false);
  const [withMaterials, setWithMaterials] = useState(false);
  const [all, setAll] = useState(false);

  // The pane is keyed by the read, so it mounts fresh after a save.
  useEffect(() => {
    let cancelled = false;
    invoke<ColorReport>("niagara_colors", { gameRoot: gamePath, container, entry }).then(
      (result) => {
        if (!cancelled) setReport(result);
      },
      (e) => {
        if (!cancelled) setError(String(e));
      }
    );
    return () => {
      cancelled = true;
    };
  }, [gamePath, container, entry]);

  const groups = useMemo(() => {
    const out: { emitter: string | null; sources: ColorSource[]; renderers: RendererColors[] }[] =
      [];
    const groupOf = (emitter: string | null) => {
      let group = out.find((held) => held.emitter === emitter);
      if (!group) {
        group = { emitter, sources: [], renderers: [] };
        out.push(group);
      }
      return group;
    };
    for (const source of report?.sources ?? [])
      groupOf(source.emitter ?? null).sources.push(source);
    for (const renderer of report?.renderers ?? []) {
      if (renderer.colors.length > 0) groupOf(renderer.emitter ?? null).renderers.push(renderer);
    }
    return out.sort((a, b) => Number(a.emitter === null) - Number(b.emitter === null));
  }, [report]);

  if (error) return <p className="p-6 text-center text-sm text-red-400">{error}</p>;
  if (!report)
    return <p className="p-6 text-center text-sm text-muted-foreground">Reading colours…</p>;
  const drawing = report.renderers.filter((renderer) => renderer.colors.length > 0);
  if (report.sources.length === 0 && drawing.length === 0) {
    return (
      <p className="p-6 text-center text-sm text-muted-foreground">
        No colour curve, array, parameter, script colour or material colour in this package.
      </p>
    );
  }
  const inActive = report.sources.filter((source) => source.export === activeExport);
  const current = inActive.find((source) => sourceKey(source) === chosen) ?? inActive[0] ?? null;
  const renderer = drawing.find((held) => held.export === activeExport) ?? null;
  const showRenderer = renderer !== null && (current === null || chosen === rendererKey(renderer));
  const editable = report.sources.filter((source) => source.kind !== "script");
  const materialColors = drawing.flatMap((held) => held.colors).filter(recolorable);
  const enemies =
    editable.filter((source) => source.enemy).length +
    (withMaterials ? materialColors.filter((color) => color.enemy).length : 0);

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
      <div className="flex flex-wrap items-center gap-3 border-b border-border/60 px-3 py-1.5 text-[11px] text-muted-foreground">
        <Palette size={12} />
        <span>
          {report.sources.length} colour source{report.sources.length === 1 ? "" : "s"}
        </span>
        {report.gpu_scripts.length > 0 && (
          <Tip content={report.gpu_scripts.join(", ")}>
            <span>
              {report.gpu_scripts.length} GPU script{report.gpu_scripts.length === 1 ? "" : "s"},
              whose own colours are shader code
            </span>
          </Tip>
        )}
        <Button
          size="sm"
          variant={all ? "secondary" : "outline"}
          className="ml-auto h-6 px-2 text-[11px]"
          aria-expanded={all}
          disabled={editable.length === 0 && materialColors.length === 0}
          onClick={() => setAll((open) => !open)}
        >
          Recolour effect
        </Button>
      </div>
      {all && (
        <div className="space-y-1.5 border-b border-border/60 px-3 py-2">
          <p className="text-[11px] text-muted-foreground">
            Changes every curve, array and parameter in the package (each script's copy of a curve
            included) in one save, and with the material colours, sets each renderer's own over its
            materials'. Script colours are left to the CLI's asset recolor --scripts.
          </p>
          <div className="flex flex-wrap items-center gap-3">
            <Tools
              label="Recolour and save"
              blocked={recolorBlocked}
              onApply={(tool) => {
                const pick: RecolorPick = {};
                if (enemyOnly) pick.enemy = true;
                if (withMaterials) pick.materials = true;
                onRecolorAll(pick, tool);
              }}
            />
            <label className="flex items-center gap-1.5 text-[11px]">
              <Checkbox
                checked={withMaterials}
                disabled={materialColors.length === 0}
                onCheckedChange={(next) => setWithMaterials(next === true)}
              />
              With the {materialColors.length} material colour
              {materialColors.length === 1 ? "" : "s"}
            </label>
            <label className="flex items-center gap-1.5 text-[11px]">
              <Checkbox
                checked={enemyOnly}
                disabled={enemies === 0}
                onCheckedChange={(next) => setEnemyOnly(next === true)}
              />
              Only the {enemies} enemy colour{enemies === 1 ? "" : "s"}
            </label>
          </div>
        </div>
      )}
      <div className="flex min-h-0 flex-1">
        <ul className="w-80 shrink-0 overflow-auto border-r border-border/60 py-1 text-[11px]">
          {groups.map((group) => (
            <li key={group.emitter ?? ""}>
              <p className="px-3 pb-0.5 pt-2 text-[10px] font-semibold uppercase text-muted-foreground">
                {group.emitter ?? "System"}
              </p>
              <ul>
                {group.sources.map((source) => {
                  const key = sourceKey(source);
                  const on = current !== null && sourceKey(current) === key;
                  return (
                    <li key={key}>
                      <button
                        type="button"
                        aria-current={on ? "true" : undefined}
                        className={cn(
                          "flex w-full items-center gap-2 px-3 py-1 text-left hover:bg-muted/50",
                          on && "bg-blue-accent/20"
                        )}
                        onClick={() => onChoose(source.export, key)}
                      >
                        <Preview source={source} />
                        <span className="min-w-0 flex-1 truncate">{sourceLabel(source)}</span>
                        {(source.peak ?? 0) > 1 && (
                          <span className="text-muted-foreground">
                            HDR {shortFloat(source.peak ?? 0)}
                          </span>
                        )}
                        {source.enemy && <span className="text-red-400">enemy</span>}
                      </button>
                    </li>
                  );
                })}
                {group.renderers.map((held) => {
                  const key = rendererKey(held);
                  const on = showRenderer && renderer.export === held.export;
                  return (
                    <li key={key}>
                      <button
                        type="button"
                        aria-current={on ? "true" : undefined}
                        className={cn(
                          "flex w-full items-center gap-2 px-3 py-1 text-left hover:bg-muted/50",
                          on && "bg-blue-accent/20"
                        )}
                        onClick={() => onChoose(held.export, key)}
                      >
                        <span className="flex shrink-0 gap-0.5">
                          {held.colors.slice(0, 3).map((color) => (
                            <Chip
                              key={`${color.material ?? ""}:${color.name}`}
                              color={opaque(color.color)}
                              peak={peakOf(color.color)}
                            />
                          ))}
                        </span>
                        <span className="min-w-0 flex-1 truncate">
                          {held.materials.map(objectName).join(", ") || held.object_name}
                        </span>
                        {held.colors.some((color) => color.enemy) && (
                          <span className="text-red-400">enemy</span>
                        )}
                      </button>
                    </li>
                  );
                })}
              </ul>
            </li>
          ))}
        </ul>
        <div className="min-w-0 flex-1 overflow-auto p-3">
          {showRenderer ? (
            <MaterialView renderer={renderer} />
          ) : current === null ? (
            <p className="text-center text-sm text-muted-foreground">
              Pick a colour on the left. This export holds none.
            </p>
          ) : current.kind === "lut" ? (
            <LutEditor key={sourceKey(current)} source={current} properties={properties} />
          ) : current.kind === "array" ? (
            <ArrayEditor key={sourceKey(current)} source={current} properties={properties} />
          ) : current.kind === "parameter" ? (
            <ParameterEditor key={sourceKey(current)} source={current} properties={properties} />
          ) : (
            <ScriptView source={current} entry={entry} onOpenVm={() => onOpenVm(current.export)} />
          )}
        </div>
      </div>
    </div>
  );
}
