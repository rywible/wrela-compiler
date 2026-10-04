// Builds small WASM modules for tests (bun has no WAT compiler): just the sections the program
// ABI tests need.

type ValType = "i32" | "i64" | "f32" | "f64";
const VAL: Record<ValType, number> = { i32: 0x7f, i64: 0x7e, f32: 0x7d, f64: 0x7c };

export interface FuncType {
  params: ValType[];
  results: ValType[];
}

export type Import =
  | { module: string; name: string; kind: "func"; type: FuncType }
  | { module: string; name: string; kind: "memory" };

export interface Func {
  type: FuncType;
  /** The body's instructions, without locals or the final `end`. */
  body: number[];
  export?: string;
}

export interface ModuleSpec {
  imports?: Import[];
  funcs?: Func[];
    /** Pages of memory, imported shared as `wrela.memory` (or, with `ownMemory`, defined
   * unshared), and exported as `memory` unless `exportMemory` is false. */
  memory?: number;
  ownMemory?: boolean;
  exportMemory?: boolean;
  data?: { offset: number; bytes: Uint8Array }[];
  /** The start function's index (imports first). */
  start?: number;
}

const uleb = (n: number): number[] => {
  const out = [];
  do {
    let b = n & 0x7f;
    n = Math.floor(n / 128);
    if (n !== 0) b |= 0x80;
    out.push(b);
  } while (n !== 0);
  return out;
};

const sleb = (n: number): number[] => {
  const out = [];
  for (;;) {
    const b = n & 0x7f;
    n >>= 7;
    if ((n === 0 && (b & 0x40) === 0) || (n === -1 && (b & 0x40) !== 0)) {
      out.push(b);
      return out;
    }
    out.push(b | 0x80);
  }
};

const name = (s: string) => {
  const b = Array.from(new TextEncoder().encode(s));
  return [...uleb(b.length), ...b];
};
const vec = (items: number[][]) => [...uleb(items.length), ...items.flat()];
const section = (id: number, body: number[]) => (body.length ? [id, ...uleb(body.length), ...body] : []);

/** Instructions. */
export const op = {
  i32: (n: number) => [0x41, ...sleb(n | 0)],
  call: (f: number) => [0x10, ...uleb(f)],
  unreachable: [0x00],
};

export function buildModule(spec: ModuleSpec): Uint8Array<ArrayBuffer> {
  const imports = spec.imports ?? [];
  const funcs = spec.funcs ?? [];
  const types: string[] = [];
  const typeIndex = (t: FuncType) => {
    const key = JSON.stringify(t);
    if (!types.includes(key)) types.push(key);
    return types.indexOf(key);
  };
    const importEntries = imports.map((i) =>
    i.kind === "func"
      ? [...name(i.module), ...name(i.name), 0x00, ...uleb(typeIndex(i.type))]
      : [...name(i.module), ...name(i.name), 0x02, 0x00, 0x01],
  );
  // The program ABI's memory: shared, its maximum 1 GiB.
  const shared = spec.memory !== undefined && !spec.ownMemory;
  if (shared) importEntries.push([...name("wrela"), ...name("memory"), 0x02, 0x03, ...uleb(spec.memory!), ...uleb(16384)]);
  const funcEntries = funcs.map((f) => uleb(typeIndex(f.type)));
  const importedFuncs = imports.filter((i) => i.kind === "func").length;
  const exports: number[][] = [];
  if (spec.memory !== undefined && spec.exportMemory !== false) exports.push([...name("memory"), 0x02, 0x00]);
  funcs.forEach((f, i) => {
    if (f.export) exports.push([...name(f.export), 0x00, ...uleb(importedFuncs + i)]);
  });
  const typeEntries = types.map((key) => {
    const t = JSON.parse(key) as FuncType;
    return [0x60, ...vec(t.params.map((p) => [VAL[p]])), ...vec(t.results.map((r) => [VAL[r]]))];
  });
  const code = funcs.map((f) => {
    const body = [0x00, ...f.body, 0x0b]; // no locals
    return [...uleb(body.length), ...body];
  });
  const data = (spec.data ?? []).map((d) => [0x00, ...op.i32(d.offset), 0x0b, ...uleb(d.bytes.length), ...d.bytes]);
  return Uint8Array.from([
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    ...section(1, typeEntries.length ? vec(typeEntries) : []),
    ...section(2, importEntries.length ? vec(importEntries) : []),
    ...section(3, funcEntries.length ? vec(funcEntries) : []),
        ...section(5, spec.memory !== undefined && spec.ownMemory ? vec([[0x00, ...uleb(spec.memory)]]) : []),
    ...section(7, exports.length ? vec(exports) : []),
    ...section(8, spec.start !== undefined ? uleb(spec.start) : []),
    ...section(10, code.length ? vec(code) : []),
    ...section(11, data.length ? vec(data) : []),
  ]);
}

export const SUBMIT: Import = { module: "wrela", name: "submit", kind: "func", type: { params: ["i32", "i32"], results: [] } };
export const FRAME_TYPE: FuncType = { params: ["f32", "i32", "i32"], results: [] };

/**
 * A program whose `n`th call of `frame` submits `frames[n]`'s batches in order (nothing once
 * `frames` runs out), like runtime/native/tests/suite/common/wat.rs. Frame n is told apart by a
 * counter in memory word 0.
 */
export function batchProgram(frames: Uint8Array[][]): Uint8Array<ArrayBuffer> {
  const data: { offset: number; bytes: Uint8Array }[] = [];
  let offset = 16;
  const body: number[] = [];
  frames.forEach((batches, n) => {
    // if (mem[0] == n) { submit... }
    body.push(...op.i32(0), 0x28, 0x02, 0x00, ...op.i32(n), 0x46, 0x04, 0x40);
    for (const b of batches) {
      data.push({ offset, bytes: b });
      body.push(...op.i32(offset), ...op.i32(b.length), ...op.call(0));
      offset += Math.ceil(b.length / 4) * 4;
    }
    body.push(0x0b);
  });
  // mem[0] += 1
  body.push(...op.i32(0), ...op.i32(0), 0x28, 0x02, 0x00, ...op.i32(1), 0x6a, 0x36, 0x02, 0x00);
  return buildModule({
    imports: [SUBMIT],
    memory: Math.max(1, Math.ceil(offset / 65536)),
    funcs: [{ type: FRAME_TYPE, body, export: "frame" }],
    data,
  });
}
