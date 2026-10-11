// The imports of another thread's instance of the program: a helper's, the ticker's, the voice's.
// It needs only the ABI, so the audio worklet's small bundle can have it too.

import { HOST_FUNCTIONS, IMPORT_CLOCK, IMPORT_MEMORY, IMPORT_MODULE, IMPORT_PHASE, IMPORT_PRINT } from "./abi.gen.ts";

/** `wrela.clock(ptr)`: the seconds on this host's clock, an `f64`, written at `ptr`. A page's
 * workers share a time origin's scale (`performance.timeOrigin` is each context's start on one
 * clock), so every thread reads the same clock. */
export function clockImport(memory: WebAssembly.Memory): (ptr: number) => void {
  return (ptr: number) => {
    const seconds = (performance.timeOrigin + performance.now()) / 1000;
    new DataView(memory.buffer).setFloat64(ptr >>> 0, seconds, true);
  };
}

/** `wrela.print(ptr, len)`: the line at `ptr` (UTF-8), shown on the console now, prefixed so it's
 * found; it's returned for whoever else keeps it. Shared memory can't be decoded in place. */
export function printed(memory: WebAssembly.Memory, ptr: number, len: number): string {
  const line = new TextDecoder().decode(new Uint8Array(memory.buffer, ptr >>> 0, len >>> 0).slice());
  console.log(`wrela: ${line}`);
  return line;
}

/** `wrela.phase(ptr, len)`: the browser host times no phases (`std::time::phase`); a page is
 * profiled in the browser's own tools. */
export function ignorePhase(_ptr: number, _len: number): void {}

/** The shared `memory`, and host functions that refuse to run: they throw a `Refusal` that says
 * `who` can't call the host. Another thread's code records no GPU work and makes no requests (its
 * effects are checked). */
export function refusingImports(
  memory: WebAssembly.Memory,
  who: string,
  Refusal: new (message: string) => Error,
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
      [IMPORT_PRINT]: (ptr: number, len: number) => void printed(memory, ptr, len),
      [IMPORT_PHASE]: ignorePhase,
    },
  };
}
