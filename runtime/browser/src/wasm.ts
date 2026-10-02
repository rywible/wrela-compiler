// Just enough of the WASM binary format to read the types of a module's imported and exported
// functions, which JavaScript can't see (`WebAssembly.Module.imports` gives only names and
// kinds). The native host gets them from wasmtime; both check them against the program ABI.

export interface FunctionTypes {
  /** `module.name` of each imported function, to its type, e.g. `(i32, i32) -> ()`. */
  imports: Map<string, string>;
  /** Each exported function's name, to its type. */
  exports: Map<string, string>;
}

const VALTYPES: Record<number, string> = {
  0x7f: "i32",
  0x7e: "i64",
  0x7d: "f32",
  0x7c: "f64",
  0x7b: "v128",
  0x70: "funcref",
  0x6f: "externref",
};

class Reader {
  at = 0;
  constructor(readonly bytes: Uint8Array) {}

  byte(): number {
    const b = this.bytes[this.at++];
    if (b === undefined) throw new Error("unexpected end of the module");
    return b;
  }

  /** An unsigned LEB128 (up to 64 bits; precision past 2^53 doesn't matter for skipping). */
  uleb(): number {
    let result = 0;
    let scale = 1;
    for (;;) {
      const b = this.byte();
      result += (b & 0x7f) * scale;
      if ((b & 0x80) === 0) return result;
      scale *= 128;
    }
  }

  name(): string {
    const len = this.uleb();
    const s = new TextDecoder().decode(this.bytes.subarray(this.at, this.at + len));
    this.at += len;
    return s;
  }

  valtype(): string {
    const b = this.byte();
    if (b === 0x63 || b === 0x64) {
      this.uleb(); // a typed reference's heap type
      return "ref";
    }
    const t = VALTYPES[b];
    if (t === undefined) throw new Error(`unknown value type 0x${b.toString(16)}`);
    return t;
  }

  limits(): void {
    const flags = this.byte();
    this.uleb();
    if (flags & 1) this.uleb();
  }
}

/** Reads a module's function types. Throws on a module it can't read (validate it first). */
export function functionTypes(bytes: Uint8Array): FunctionTypes {
  const r = new Reader(bytes);
  r.at = 8; // magic and version
  const types: string[] = [];
  const funcs: string[] = []; // the type of each function index: imports first
  const out: FunctionTypes = { imports: new Map(), exports: new Map() };
  while (r.at < bytes.length) {
    const id = r.byte();
    const size = r.uleb();
    const end = r.at + size;
    if (id === 1) {
      for (let n = r.uleb(); n > 0; n--) {
        const form = r.byte();
        if (form !== 0x60) throw new Error(`unsupported type form 0x${form.toString(16)}`);
        const params = Array.from({ length: r.uleb() }, () => r.valtype());
        const results = Array.from({ length: r.uleb() }, () => r.valtype());
        types.push(`(${params.join(", ")}) -> (${results.join(", ")})`);
      }
    } else if (id === 2) {
      for (let n = r.uleb(); n > 0; n--) {
        const name = `${r.name()}.${r.name()}`;
        const kind = r.byte();
        if (kind === 0) {
          const ty = types[r.uleb()] ?? "(?)";
          funcs.push(ty);
          out.imports.set(name, ty);
        } else if (kind === 1) {
          r.valtype();
          r.limits();
        } else if (kind === 2) {
          r.limits();
        } else if (kind === 3) {
          r.valtype();
          r.byte();
        } else if (kind === 4) {
          r.byte();
          r.uleb();
        } else {
          throw new Error(`unknown import kind ${kind}`);
        }
      }
    } else if (id === 3) {
      for (let n = r.uleb(); n > 0; n--) funcs.push(types[r.uleb()] ?? "(?)");
    } else if (id === 7) {
      for (let n = r.uleb(); n > 0; n--) {
        const name = r.name();
        const kind = r.byte();
        const index = r.uleb();
        if (kind === 0) out.exports.set(name, funcs[index] ?? "(?)");
      }
    }
    r.at = end;
  }
  return out;
}
