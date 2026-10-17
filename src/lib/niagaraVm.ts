/** What a constant operand of a Niagara VM program reads. */
export interface VmConst {
  offset: number;
  table: string;
  within: number;
  name?: string;
  bits?: number;
  key: string;
}

export interface VmLine {
  at: number;
  text: string;
  constants: [number, VmConst][];
  writes?: string;
}

export type VmLeaf =
  | {
      kind: "constant";
      at: number;
      operand: number;
      constant: VmConst;
      ty: "float" | "int" | "any";
      uses: number;
      feeds: string[];
    }
  | { kind: "input"; at: number; name: string }
  | { kind: "call"; at: number; function: string; output: number }
  | { kind: "index"; at: number; what: string }
  | { kind: "unwritten"; register: number };

export interface VmTrace {
  at: number;
  writes: string;
  expression: string;
  leaves: VmLeaf[];
  baked: boolean;
}

/** A Niagara script's VM program as `export_vm_view` returns it. */
export interface VmView {
  object_name: string;
  usage: string;
  interpolated: boolean;
  param_size: number;
  bytecode_len: number;
  literals: number[];
  program: { instructions: unknown[]; stop?: { at: number; reason: string } };
  layout?: { tables: [string, number, number][] };
  lines: VmLine[];
  traces: VmTrace[];
  edit_refusal?: string;
}

export const hex4 = (value: number) => `0x${value.toString(16).toUpperCase().padStart(4, "0")}`;

/** How four bytes read as the operand's type: a float, an int, or both spelled as hex. */
export function showBits(bits: number, ty: "float" | "int" | "any"): string {
  const view = new DataView(new ArrayBuffer(4));
  view.setUint32(0, bits, true);
  if (ty === "float") {
    const value = view.getFloat32(0, true);
    return Number.isInteger(value) ? value.toFixed(1) : String(Number(value.toPrecision(7)));
  }
  if (ty === "int") return String(view.getInt32(0, true));
  return `0x${bits.toString(16).toUpperCase().padStart(8, "0")}`;
}

export function constantText(leaf: Extract<VmLeaf, { kind: "constant" }>): string {
  const { constant } = leaf;
  if (constant.name) return constant.name;
  if (constant.bits !== undefined) return showBits(constant.bits, leaf.ty);
  return constant.key;
}

/** The CLI line that gives a constant leaf a new value, with its current one to change. */
export function editCommand(
  entry: string,
  script: string,
  leaf: Extract<VmLeaf, { kind: "constant" }>
): string {
  const value =
    leaf.constant.bits !== undefined
      ? showBits(leaf.constant.bits, leaf.ty === "any" ? "float" : leaf.ty)
      : "0.0";
  const typed = leaf.ty === "any" ? `float:${value}` : value;
  return `rivals-cli asset edit-vm "${entry}" --export ${script} --at ${hex4(leaf.at)} --operand ${leaf.operand} --value ${typed} --into MyMod`;
}
