// The imports of another thread's instance of the program: a helper's, the ticker's, the voice's.
// It needs only the ABI, so the audio worklet's small bundle can have it too.

import { HOST_FUNCTIONS, IMPORT_CLOCK, IMPORT_MEMORY, IMPORT_MODULE, IMPORT_PHASE, IMPORT_PRINT, PANIC, PANIC_CAP, THREAD_BLOCK_SIZE, THREAD_BLOCKS } from "./abi.gen.ts";

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

/** `wrela.phase(ptr, len)` on a thread whose phases aren't timed (a parallel job's, a voice's):
 * it reads and validates the name, as the ABI does. The program's own thread times them
 * ([`Phases`]). */
export function phaseImport(memory: WebAssembly.Memory): (ptr: number, len: number) => string {
  return (ptr: number, len: number) => text(memory, ptr, len, "phase");
}

/** The phases a thread times (`std::time::phase`), as the native host's trace does: a phase runs
 * until the thread's next one, or until its call (`init`, `frame`) returns, and its time is summed
 * by name over the call. `take` gives what the calls since the last `take` timed, ms by name. */
export class Phases {
  #current: string | null = null;
  #since = 0;
  #times = new Map<string, number>();

  constructor(readonly now: () => number = () => performance.now()) {}

  /** Starts phase `name`, ending the last; an empty name only ends it, as on the native host. */
  begin(name: string): void {
    this.end();
    if (name === "") return;
    this.#current = name;
    this.#since = this.now();
  }

  end(): void {
    if (this.#current === null) return;
    this.#times.set(this.#current, (this.#times.get(this.#current) ?? 0) + this.now() - this.#since);
    this.#current = null;
  }

  take(): Record<string, number> {
    const out = Object.fromEntries(this.#times);
    this.#times.clear();
    return out;
  }
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

/** The message a panic on thread `thread` left before it trapped (its block's `PANIC`), cleared
 * so a later trap that isn't a panic doesn't repeat it. */
export function takePanicMessage(memory: WebAssembly.Memory, thread: number): string | null {
  const at = THREAD_BLOCKS + thread * THREAD_BLOCK_SIZE + PANIC;
  // A memory too small to hold the block (not a compiled program's) left no message there.
  if (at + 4 + PANIC_CAP > memory.buffer.byteLength) return null;
  const view = new DataView(memory.buffer);
  const count = Math.min(view.getUint32(at, true), PANIC_CAP);
  if (count === 0) return null;
  // A copy: TextDecoder doesn't read shared memory.
  const bytes = new Uint8Array(memory.buffer, at + 4, count).slice();
  view.setUint32(at, 0, true);
  return new TextDecoder().decode(bytes);
}
