// Just enough of the WASM binary format to read the types of a module's imported and exported
// functions, which JavaScript can't see (`WebAssembly.Module.imports` gives only names and
// kinds). The native host gets them from wasmtime; both check them against the program ABI.

export interface FunctionTypes {
  /** Each import's type in import order, e.g. `(i32, i32) -> ()`; null if it isn't a function.
   * In order, not by name: a module may import one name twice with different types. */
  imports: (string | null)[];
  /** Each exported function's name, to its type. */
  exports: Map<string, string>;
  /** Each imported memory, by import index: its limits in pages, and whether it's shared. */
  memories: Map<number, MemoryLimits>;
}

export interface MemoryLimits {
  initial: number;
  maximum: number | null;
  shared: boolean;
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

/** Strict UTF-8 that keeps a leading byte-order mark, as Rust's `str::from_utf8` does. */
export const UTF8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/** Reads WASM's encodings in order; `eof` is the message when the bytes run out. */
export class ByteReader {
  at = 0;
  constructor(
    readonly bytes: Uint8Array,
    readonly eof = "unexpected end of the module",
  ) {}

  byte(): number {
    const b = this.bytes[this.at++];
    if (b === undefined) throw new Error(this.eof);
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

  /** An unsigned LEB128 that fits in 32 bits, in at most 5 bytes. */
  u32(): number {
    let x = 0;
    for (let shift = 0; shift < 35; shift += 7) {
      const b = this.byte();
      x += (b & 0x7f) * 2 ** shift;
      if ((b & 0x80) === 0) {
        if (x > 0xffffffff) throw new Error("a number is too large");
        return x;
      }
    }
    throw new Error("a number is too long");
  }

  /** The next `n` bytes. */
  take(n: number): Uint8Array {
    if (this.at + n > this.bytes.length) throw new Error(this.eof);
    const out = this.bytes.subarray(this.at, this.at + n);
    this.at += n;
    return out;
  }

  /** A length-prefixed UTF-8 string. */
  name(): string {
    return UTF8.decode(this.take(this.u32()));
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

    /** Limits: 0x01 has a maximum, 0x02 shared. */
  limits(): MemoryLimits {
    const flags = this.byte();
    const initial = this.uleb();
    const maximum = flags & 1 ? this.uleb() : null;
    return { initial, maximum, shared: (flags & 2) !== 0 };
  }
}

/** Reads a module's function types. Throws on a module it can't read (validate it first). */
export function functionTypes(bytes: Uint8Array): FunctionTypes {
  const r = new ByteReader(bytes);
  r.at = 8; // magic and version
  const types: string[] = [];
  const funcs: string[] = []; // the type of each function index: imports first
    const out: FunctionTypes = { imports: [], exports: new Map(), memories: new Map() };
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
        r.name(); // module
        r.name(); // name
        const kind = r.byte();
        if (kind === 0) {
          const ty = types[r.uleb()] ?? "(?)";
          funcs.push(ty);
          out.imports.push(ty);
          continue;
        }
                out.imports.push(null);
        if (kind === 1) {
          r.valtype();
          r.limits();
        } else if (kind === 2) {
          out.memories.set(out.imports.length - 1, r.limits());
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
