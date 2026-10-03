// The `wrela.lines` custom section of a program's WASM: where in the source each part of its
// code came from, so a trap can say where it happened. Mirrors runtime/abi/src/lines.rs (the
// format is described there).

export const LINES_SECTION = "wrela.lines";

/** A decoded table: module offsets, ascending, each with a location or none. */
export class Lines {
  private constructor(
    readonly locations: string[],
    readonly entries: [offset: number, location: number][],
  ) {}

  /** The location of the code at module offset `offset`, if it has one. */
  at(offset: number): string | null {
    let lo = 0;
    let hi = this.entries.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (this.entries[mid]![0] <= offset) lo = mid + 1;
      else hi = mid;
    }
    if (lo === 0) return null;
    const k = this.entries[lo - 1]![1];
    return k > 0 ? this.locations[k - 1]! : null;
  }

  /** Reads a section's payload. */
  static decode(payload: Uint8Array): Lines {
    let at = 0;
    const leb = (): number => {
      let x = 0;
      for (let shift = 0; shift < 35; shift += 7) {
        const b = payload[at++];
        if (b === undefined) throw new Error("the section ends early");
        x += (b & 0x7f) * 2 ** shift;
        if ((b & 0x80) === 0) {
          if (x > 0xffffffff) throw new Error("a number is too large");
          return x;
        }
      }
      throw new Error("a number is too long");
    };
    const locations: string[] = [];
    const decoder = new TextDecoder("utf-8", { fatal: true });
    for (let i = leb(); i > 0; i--) {
      const n = leb();
      if (at + n > payload.length) throw new Error("the section ends early");
      locations.push(decoder.decode(payload.subarray(at, at + n)));
      at += n;
    }
    const entries: [number, number][] = [];
    let last = -1;
    for (let i = leb(); i > 0; i--) {
      const offset = leb();
      const k = leb();
      if (k > locations.length) throw new Error(`entry at ${offset} names location ${k}, of ${locations.length}`);
      if (offset < last) throw new Error(`the entries aren't in order at ${offset}`);
      last = offset;
      entries.push([offset, k]);
    }
    if (at !== payload.length) throw new Error("bytes after the entries");
    return new Lines(locations, entries);
  }

  /** The table in a compiled module, if it has one. */
  static of(module: WebAssembly.Module): Lines | null {
    const [section] = WebAssembly.Module.customSections(module, LINES_SECTION);
    return section ? Lines.decode(new Uint8Array(section)) : null;
  }
}

/**
 * The module offsets of the WASM frames of a stack trace, innermost first, as V8 and
 * SpiderMonkey write them (`wasm-function[12]:0x1a2b`). Engines that don't write offsets give
 * none.
 */
export function wasmOffsets(stack: string): number[] {
  return [...stack.matchAll(/wasm-function\[\d+\]:0x([0-9a-f]+)/gi)].map((m) => Number.parseInt(m[1]!, 16));
}
