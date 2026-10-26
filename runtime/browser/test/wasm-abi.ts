// The types of a module's imported and exported functions, read from its bytes: what a build
// states in its manifest's `wasm_abi` (the compiler reads them with wasmparser). The tests make
// a module's `wasm_abi` from its bytes with this.

import type { MemoryLimits, WasmAbi } from "../src/manifest.ts";
import type { Checker } from "../src/check.ts";
import { type Executor, Program, type ProgramOptions } from "../src/program.ts";
import type { Bytes } from "../src/stream.ts";
import { ByteReader } from "../src/wasm.ts";

const VALTYPES: Record<number, string> = {
  0x7f: "i32",
  0x7e: "i64",
  0x7d: "f32",
  0x7c: "f64",
  0x7b: "v128",
  0x70: "funcref",
  0x6f: "externref",
};

function valtype(r: ByteReader): string {
  const b = r.byte();
  if (b === 0x63 || b === 0x64) {
    r.uleb(); // a typed reference's heap type
    return "ref";
  }
  const t = VALTYPES[b];
  if (t === undefined) throw new Error(`unknown value type 0x${b.toString(16)}`);
  return t;
}

/** Limits: 0x01 has a maximum, 0x02 shared. */
function limits(r: ByteReader): MemoryLimits {
  const flags = r.byte();
  const initial = r.uleb();
  const maximum = flags & 1 ? r.uleb() : null;
  return { initial, maximum, shared: (flags & 2) !== 0 };
}

const KINDS = ["function", "table", "memory", "global", "tag"];

/** A module's interface as its build's manifest states it. Throws on a module it can't read. */
export function abiOf(bytes: Uint8Array): WasmAbi {
  const r = new ByteReader(bytes);
  r.at = 8; // magic and version
  const types: string[] = [];
  const funcs: string[] = []; // the type of each function index: imports first
  const abi: WasmAbi = { memory: null, imports: [], exports: [] };
  while (r.at < bytes.length) {
    const id = r.byte();
    const size = r.uleb();
    const end = r.at + size;
    if (id === 1) {
      for (let n = r.uleb(); n > 0; n--) {
        const form = r.byte();
        if (form !== 0x60) throw new Error(`unsupported type form 0x${form.toString(16)}`);
        const params = Array.from({ length: r.uleb() }, () => valtype(r));
        const results = Array.from({ length: r.uleb() }, () => valtype(r));
        types.push(`(${params.join(", ")}) -> (${results.join(", ")})`);
      }
    } else if (id === 2) {
      for (let n = r.uleb(); n > 0; n--) {
        const module = r.name();
        const name = r.name();
        const kind = r.byte();
        let ty = KINDS[kind];
        if (ty === undefined) throw new Error(`unknown import kind ${kind}`);
        if (kind === 0) {
          ty = types[r.uleb()] ?? "(?)";
          funcs.push(ty);
        } else if (kind === 1) {
          valtype(r);
          limits(r);
        } else if (kind === 2) {
          const l = limits(r);
          if (module === "wrela" && name === "memory") abi.memory = l;
        } else if (kind === 3) {
          valtype(r);
          r.byte();
        } else {
          r.byte();
          r.uleb();
        }
        abi.imports.push([module, name, ty]);
      }
    } else if (id === 3) {
      for (let n = r.uleb(); n > 0; n--) funcs.push(types[r.uleb()] ?? "(?)");
    } else if (id === 7) {
      for (let n = r.uleb(); n > 0; n--) {
        const name = r.name();
        const kind = r.byte();
        const index = r.uleb();
        if (kind === 0) abi.exports.push([name, funcs[index] ?? "(?)"]);
      }
    }
    r.at = end;
  }
  return abi;
}

/** `Program.load` with the module's `wasm_abi` read from its bytes, as its build would state it
 * (null where the bytes don't read: compiling them fails first). */
export function loadProgram(wasm: Bytes, checker: Checker, executor: Executor, options?: ProgramOptions): Promise<Program> {
  let abi: WasmAbi | null = null;
  try {
    abi = abiOf(wasm);
  } catch {
    abi = null;
  }
  return Program.load(wasm, abi, checker, executor, options);
}
