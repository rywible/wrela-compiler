// The imports of another thread's instance of the program: a helper's, the ticker's, the voice's.
// It needs only the ABI, so the audio worklet's small bundle can have it too.

import { HOST_FUNCTIONS, IMPORT_CLOCK, IMPORT_MEMORY, IMPORT_MODULE, IMPORT_PHASE, IMPORT_PRINT } from "./abi.gen.ts";
import { UTF8 } from "./wasm.ts";

/** `wrela.clock(ptr)`: the seconds on this host's clock, an `f64`, written at `ptr`. A page's
 * workers share a time origin's scale (`performance.timeOrigin` is each context's start on one
 * clock), so every thread reads the same clock. */
export function clockImport(memory: WebAssembly.Memory): (ptr: number) => void {
  return (ptr: number) => {
    const seconds = (performance.timeOrigin + performance.now()) / 1000;
    new DataView(memory.buffer).setFloat64(ptr >>> 0, seconds, true);
  };
}

/** The strict UTF-8 at `ptr..ptr + len`, copied out of shared memory. */
function text(memory: WebAssembly.Memory, ptr: number, len: number, what: string): string {
  const [at, size] = [ptr >>> 0, len >>> 0];
  let bytes: Uint8Array;
  try {
    // TextDecoder can't read shared memory in place.
    bytes = new Uint8Array(memory.buffer, at, size).slice();
  } catch {
    throw new Error(`${what}(${at}, ${size}) reaches past the end of the program's memory`);
  }
  try {
    return UTF8.decode(bytes);
  } catch {
    throw new Error(`${what}: the text isn't UTF-8`);
  }
}

/** `wrela.print(ptr, len)`: the line at `ptr` (UTF-8), shown on the console now, prefixed so it's
 * found; it's returned for whoever else keeps it. */
export function printed(memory: WebAssembly.Memory, ptr: number, len: number): string {
  const line = text(memory, ptr, len, "print");
  console.log(`wrela: ${line}`);
  return line;
}

/** `wrela.phase(ptr, len)`: the browser host times no phases (`std::time::phase`); a page is
 * profiled in the browser's own tools. It still reads and validates the name, as the ABI does. */
export function phaseImport(memory: WebAssembly.Memory): (ptr: number, len: number) => void {
  return (ptr: number, len: number) => void text(memory, ptr, len, "phase");
}

/** The shared `memory`, and host functions that refuse to run: they throw a `Refusal` that says
 * `who` can't call the host. Another thread's code records no GPU work and makes no requests (its
 * effects are checked). */
export function refusingImports(
  memory: WebAssembly.Memory,
  who: string,
  Refusal: new (message: string) => Error,
  onPrint: (line: string) => void = () => {},
): WebAssembly.Imports {
  const refuse = () => {
    throw new Refusal(`${who} can't call the host`);
  };
  // The clock, a line printed and a phase answer on every thread (a voice may read the clock;
  // a tick and a parallel job's chunk may print).
  return {
    [IMPORT_MODULE]: {
      [IMPORT_MEMORY]: memory,
      ...Object.fromEntries(HOST_FUNCTIONS.map(([name]) => [name, refuse])),
      [IMPORT_CLOCK]: clockImport(memory),
      [IMPORT_PRINT]: (ptr: number, len: number) => onPrint(printed(memory, ptr, len)),
      [IMPORT_PHASE]: phaseImport(memory),
    },
  };
}
