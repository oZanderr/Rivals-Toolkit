import type { VmTrace } from "@/lib/niagaraVm";

export type Rgba = [number, number, number, number];

export interface ParamCopy {
  store: string;
  offsets: number[];
}

export interface ScriptChannel extends VmTrace {
  channel: string;
  value?: number;
}

export type ColorBody =
  | {
      kind: "lut";
      samples: Rgba[];
      min_time: number;
      max_time: number;
      use_lut: boolean;
      keys: number;
      exposed_texture?: string;
    }
  | { kind: "array"; field: string; colors: Rgba[] }
  | { kind: "parameter"; name: string; color: Rgba; copies: ParamCopy[] }
  | {
      kind: "script";
      usage: string;
      attribute: string;
      /** Whether a renderer draws it; one none draws is a value others are worked out from. */
      drawn: boolean;
      channels: ScriptChannel[];
    };

/** One place a Niagara package's colours come from, as `niagara_colors` lists it. */
export type ColorSource = {
  export: number;
  object_name: string;
  class_name: string;
  emitter?: string;
  owner?: string;
  peak?: number;
  grey: boolean;
  enemy: boolean;
} & ColorBody;

/** A colour parameter a renderer draws one of its materials with. */
export interface MaterialColor {
  name: string;
  color: Rgba;
  /** The material it is drawn with; absent for the renderer's own value, set over every material's. */
  material?: string;
  /** That material or the parent of it that sets the value. */
  from?: string;
  enemy: boolean;
  /** The variable the renderer binds it to, which sets it every frame in place of this value. */
  bound?: string;
  /** A material layer's parameter, which the renderer's own values do not reach. */
  layered?: boolean;
}

/** A renderer's materials and the colours they draw with. */
export interface RendererColors {
  export: number;
  object_name: string;
  class_name: string;
  emitter?: string;
  materials: string[];
  colors: MaterialColor[];
  unread?: string[];
}

export interface ColorReport {
  sources: ColorSource[];
  gpu_scripts: string[];
  renderers: RendererColors[];
}

export type Recolor =
  | { tool: "tint"; rgb: [number, number, number] }
  | { tool: "paint"; rgb: [number, number, number] }
  | { tool: "hue"; degrees: number };

export interface RecolorPick {
  exports?: number[];
  param?: string;
  enemy?: boolean;
  /** Also the colours the renderers' materials draw with, each set on its renderer. */
  materials?: boolean;
}

export function rendererKey(renderer: RendererColors): string {
  return `${renderer.export}:materials`;
}

/** The last part of an object path: the asset's or the subobject's own name. */
export function objectName(path: string): string {
  return path.split(/[.:]/).pop() ?? path;
}

/** Whether a recolour can change a material colour: the renderer's binding sets a bound one every
 *  frame, and its values never reach a layer's. */
export function recolorable(color: MaterialColor): boolean {
  return !color.bound && !color.layered;
}

/** Tells sources apart across reads of the package: an export holds one curve or array, but
 *  several parameters and script colours. */
export function sourceKey(source: ColorSource): string {
  switch (source.kind) {
    case "parameter":
      return `${source.export}:parameter:${source.name}`;
    case "script":
      return `${source.export}:script:${source.attribute}`;
    default:
      return `${source.export}:${source.kind}`;
  }
}

export function sourceLabel(source: ColorSource): string {
  switch (source.kind) {
    case "parameter":
      return source.name;
    case "script":
      return `${source.object_name} writes ${source.attribute}`;
    default:
      return source.owner ? `${source.owner}: ${source.object_name}` : source.object_name;
  }
}

/** A colour as CSS, scaled down by `peak` where it is HDR so its hue still shows, over its alpha. */
export function cssColor([r, g, b, a]: Rgba, peak = 1): string {
  const scale = peak > 1 ? peak : 1;
  const channel = (value: number) => Math.round(Math.min(Math.max(value / scale, 0), 1) * 255);
  return `rgba(${channel(r)}, ${channel(g)}, ${channel(b)}, ${Math.min(Math.max(a, 0), 1)})`;
}

/** A gradient through `colors`, evenly spaced as a curve's samples are. */
export function cssGradient(colors: Rgba[], peak = 1): string {
  if (colors.length === 0) return "transparent";
  if (colors.length === 1) return cssColor(colors[0], peak);
  const stops = colors.map(
    (color, at) => `${cssColor(color, peak)} ${((at / (colors.length - 1)) * 100).toFixed(2)}%`
  );
  return `linear-gradient(to right, ${stops.join(", ")})`;
}

export function rgbOf(hex: string): [number, number, number] {
  const value = Number.parseInt(hex.replace("#", ""), 16);
  return [((value >> 16) & 255) / 255, ((value >> 8) & 255) / 255, (value & 255) / 255];
}

/** A float as a short label: three places, trailing zeros dropped. */
export function shortFloat(value: number): string {
  return String(Number(value.toFixed(3)));
}
