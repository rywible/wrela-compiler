// The imports of another thread's instance of the program: a helper's, the ticker's, the voice's.
// It needs only the ABI, so the audio worklet's small bundle can have it too.

import { HOST_FUNCTIONS, IMPORT_CLOCK, IMPORT_MEMORY, IMPORT_MODULE, IMPORT_PHASE, IMPORT_PRINT } from "./abi.gen.ts";

/** A Wasm start function runs while an instance is being made, before the host can safely
 * expose any of its effects. This is the same program-ABI error as the native host reports. */
export const CALLED_WHILE_STARTING =
  "its start function called the host; a wrela program calls the host only from an export";

// AudioWorkletGlobalScope has no TextDecoder. Other threads reuse a decoder, made only when
// text is read; the worklet uses ECMAScript's strict UTF-8 decoding instead.
let utf8: TextDecoder | null = null;

function decodeUtf8(bytes: Uint8Array): string {
  if (typeof TextDecoder !== "undefined") {
    return (utf8 ??= new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })).decode(bytes);
  }
  let encoded = "";
  for (const byte of bytes) encoded += `%${byte.toString(16).padStart(2, "0")}`;
  return decodeURIComponent(encoded);
}

/** `wrela.clock(ptr)`: the seconds on this host's clock, an `f64`, written at `ptr`. A page's
 * workers share a time origin's scale (`performance.timeOrigin` is each context's start on one
 * clock), so every thread reads the same clock. */
export function clockImport(memory: WebAssembly.Memory): (ptr: number) => void {
  let view: DataView | null = null;
  return (ptr: number) => {
    const seconds = (performance.timeOrigin + performance.now()) / 1000;
    // A grow replaces the buffer (and detaches an unshared one); otherwise reuse the view.
    const buffer = memory.buffer;
    const at = ptr >>> 0;
    if (at + 8 > buffer.byteLength) throw new RangeError(`clock(${at}) reaches past the end of the program's memory`);
    if (view?.buffer !== buffer) view = new DataView(buffer);
    view.setFloat64(at, seconds, true);
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
    return decodeUtf8(bytes);
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
  started: () => boolean = () => true,
  failed: (error: Error) => void = () => {},
): WebAssembly.Imports {
  let failure: Error | null = null;
  const refuse = () => {
    throw new Refusal(`${who} can't call the host`);
  };
  const afterStart = <A extends unknown[], R>(f: (...args: A) => R) => (...args: A): R => {
    if (failure !== null) throw failure;
    try {
      if (!started()) throw new Refusal(CALLED_WHILE_STARTING);
      return f(...args);
    } catch (e) {
      failure = e instanceof Error ? e : new Refusal(String(e));
      failed(failure);
      throw e;
    }
  };
  // The clock, a line printed and a phase answer on every thread (a voice may read the clock;
  // a tick and a parallel job's chunk may print).
  return {
    [IMPORT_MODULE]: {
      [IMPORT_MEMORY]: memory,
      ...Object.fromEntries(HOST_FUNCTIONS.map(([name]) => [name, afterStart(refuse)])),
      [IMPORT_CLOCK]: afterStart(clockImport(memory)),
      [IMPORT_PRINT]: afterStart((ptr: number, len: number) => onPrint(printed(memory, ptr, len))),
      [IMPORT_PHASE]: afterStart(phaseImport(memory)),
    },
  };
}
