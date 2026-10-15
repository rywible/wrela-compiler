// The `wrela.lines` custom section of a program's WASM: where in the source each part of its
// code came from, so a trap can say where it happened. Mirrors runtime/abi/src/lines.rs (the
// format is described there). An optimized build keeps the table in a file beside its WASM
// (`game.lines`), fetched only when a trap needs it: the trap's message carries its code
// offsets (`unlocated`) until the page reports it (`locate`).

import { ByteReader, UTF8 } from "./wasm.ts";

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
    const r = new ByteReader(payload, "the section ends early");
    const locations: string[] = [];
    for (let i = r.u32(); i > 0; i--) {
      const bytes = r.take(r.u32());
      try {
        locations.push(UTF8.decode(bytes));
      } catch {
        throw new Error("a location isn't UTF-8");
      }
    }
    const entries: [number, number][] = [];
    let last = -1;
    for (let i = r.u32(); i > 0; i--) {
      const offset = r.u32();
      const k = r.u32();
      if (k > locations.length) throw new Error(`entry at ${offset} names location ${k}, of ${locations.length}`);
      if (offset < last) throw new Error(`the entries aren't in order at ${offset}`);
      last = offset;
      entries.push([offset, k]);
    }
    if (r.at !== payload.length) throw new Error("bytes after the entries");
    return new Lines(locations, entries);
  }

  /** The table in a compiled module, if it has one. */
  static of(module: WebAssembly.Module): Lines | null {
    const [section] = WebAssembly.Module.customSections(module, LINES_SECTION);
    return section ? Lines.decode(new Uint8Array(section)) : null;
  }
}

export const LINES_SIDECAR = "lines";

/** How many frames' offsets a trap without its table carries, innermost first. */
const UNLOCATED_FRAMES = 16;

/** Where a trap happened, in a message, when the program's table isn't at hand: its frames'
 * module offsets, innermost first, for `locate` to look up. */
export function unlocated(offsets: number[]): string {
  return `wasm ${offsets.slice(0, UNLOCATED_FRAMES).map((o) => `0x${o.toString(16)}`).join(",")}`;
}

/**
 * `text` with the offsets a trap carries (`unlocated`) replaced by the location of the
 * innermost that has one in the table `load` gives; with the innermost offset alone, if it
 * gives none or fails.
 */
export async function locate(text: string, load: () => Promise<Lines | null>): Promise<string> {
  const found = / at wasm (0x[0-9a-f]+(?:,0x[0-9a-f]+)*)/.exec(text);
  if (found === null) return text;
  const offsets = found[1]!.split(",").map((o) => Number.parseInt(o, 16));
  let lines: Lines | null = null;
  try {
    lines = await load();
  } catch {
    // The report goes on without the locations.
  }
  const at = offsets.map((o) => lines?.at(o) ?? null).find((loc) => loc !== null);
  const where = at ?? `code offset 0x${offsets[0]!.toString(16)}`;
  return text.slice(0, found.index) + ` at ${where}` + text.slice(found.index + found[0].length);
}

/**
 * The module offsets of the WASM frames of a stack trace, innermost first, as V8 and
 * SpiderMonkey write them (`wasm-function[12]:0x1a2b`). Engines that don't write offsets give
 * none.
 */
export function wasmOffsets(stack: string): number[] {
  return [...stack.matchAll(/wasm-function\[\d+\]:0x([0-9a-f]+)/gi)].map((m) => Number.parseInt(m[1]!, 16));
}
