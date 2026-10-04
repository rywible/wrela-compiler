// The CPU side: a program's WASM, its imports, the batches it submits, and its requests.
// Mirrors runtime/native/src/program.rs: the same ABI checks before instantiating, the same order
// of work for each batch (hash, decode, sequence, check, execute), the same errors. A request is
// answered when its answer arrives (a readback, storage, a fetch: all asynchronous here), and
// the program sees the answer from its next call on (language.md §6.15).

import {
  EXPORT_AUDIO,
  EXPORT_FRAME,
  EXPORT_INIT,
  EXPORT_MEMORY,
  EXPORT_WORKER,
  HOST_FUNCTIONS,
  IMPORT_MEMORY,
  MAX_WORKERS,
  WORKERS_DONE,
  WORKERS_DONE_FAILED,
  WORKERS_FAILED,
  WORKERS_HELPED,
  IMPORT_AUDIO,
  IMPORT_LIMIT,
  IMPORT_MODULE,
  IMPORT_REQUEST_STATUS,
  IMPORT_REQUEST_TAKE,
  IMPORT_SUBMIT,
  PANIC_CAP,
  PANIC_MESSAGE,
  REQUEST_FAILED,
  REQUEST_PENDING,
} from "./abi.gen.ts";
import { type Checker, CommandError } from "./check.ts";
import { errorMessage } from "./errors.ts";
import { StateHash } from "./hash.ts";
import { Lines, LINES_SECTION, wasmOffsets } from "./lines.ts";
import { type Bytes, type Command, decode, type OpcodeName, Sequencer } from "./stream.ts";
import type { VoiceOptions } from "./messages.ts";
import { functionTypes, type MemoryLimits } from "./wasm.ts";

/** The WASM module is invalid or breaks the program ABI. */
export class ProgramError extends Error {
  constructor(why: string) {
    super(`invalid program: ${why}`);
    this.name = "ProgramError";
  }
}

/** The program trapped, or called the host with arguments it can't use. */
export class TrapError extends Error {
  constructor(why: string) {
    super(`the program trapped: ${why}`);
    this.name = "TrapError";
  }
}

/** Carries out commands that have already been decoded, sequenced and checked. */
export interface Executor {
  execute(cmd: Command): void;
  /** Called after each call of `frame` returns: submit the frame's work. */
  flush(): void;
  /** A buffer's bytes, for a `ReadBuffer` request, once the GPU's work recorded so far is
   * done. An executor without a GPU doesn't have it. */
  readBack?(handle: number, offset: number, size: number): Promise<Bytes>;
}

/** Where a program's requests read and write: its storage (bytes at paths) and its build. */
export interface Io {
  storageRead(path: string): Promise<Bytes>;
  storageWrite(path: string, data: Bytes): Promise<void>;
  fetch(url: string): Promise<Bytes>;
}

/** An answer: the bytes, or why the request failed. */
type Answer = { bytes: Bytes } | { error: string };

/** The program's requests: in flight, arrived during or between calls (seen from the next
 * call), and answered but not taken. */
class Requests {
  readonly #inFlight = new Set<number>();
  readonly #arriving: [number, Answer][] = [];
  readonly answered = new Map<number, Answer>();

  /** Starts request `request`, whose answer `work` gives. */
  start(op: OpcodeName, request: number, work: () => Promise<Bytes>): void {
    if (this.#inFlight.has(request) || this.answered.has(request) || this.#arriving.some((a) => a[0] === request)) {
      throw new CommandError(op, `request ${request} is already in use`);
    }
    this.#inFlight.add(request);
    let promise: Promise<Bytes>;
    try {
      promise = work();
    } catch (e) {
      promise = Promise.reject(e);
    }
    promise.then(
      (bytes) => this.#arrive(request, { bytes }),
      (e: unknown) => this.#arrive(request, { error: errorMessage(e) }),
    );
  }

  #arrive(request: number, answer: Answer): void {
    this.#inFlight.delete(request);
    this.#arriving.push([request, answer]);
  }

  /** The start of a call: what arrived before it is answered now. */
  settle(): void {
    for (const [r, a] of this.#arriving.splice(0)) this.answered.set(r, a);
  }
}

/** The imports a program may have, with their types as both hosts word them. */
const IMPORTS = new Map<string, string>(HOST_FUNCTIONS);

/**
 * Checks a module against the program ABI before instantiating it: it imports its memory,
 * shared, as `wrela.memory`, may import only the host's functions (`IMPORTS`) besides, must
 * export `memory` and `frame(f32, i32, i32)`. (It may have a start function, which may not call
 * the host: `#starting`.) `bytes` is the module's binary, for what JavaScript can't otherwise
 * see. The memory's limits.
 */
export function checkAbi(module: WebAssembly.Module, bytes: Uint8Array): MemoryLimits {
  let types: ReturnType<typeof functionTypes>;
  try {
    types = functionTypes(bytes);
  } catch (e) {
    throw new ProgramError(`can't read its function types: ${errorMessage(e)}`);
  }
    let memory: MemoryLimits | null = null;
  WebAssembly.Module.imports(module).forEach((imp, i) => {
    const name = `${imp.module}.${imp.name}`;
    if (imp.module === IMPORT_MODULE && imp.name === IMPORT_MEMORY) {
      const limits = types.memories.get(i);
      if (imp.kind !== "memory" || !limits?.shared) {
        const what = imp.kind === "function" ? `a function ${types.imports[i]}` : `a ${imp.kind}`;
        throw new ProgramError(`its import \`${name}\` must be a shared memory, not ${what}`);
      }
      memory = limits;
      return;
    }
    const want = imp.module === IMPORT_MODULE ? IMPORTS.get(imp.name) : undefined;
    if (want === undefined) {
      const all = [IMPORT_MEMORY, ...IMPORTS.keys()].map((n) => `\`${IMPORT_MODULE}.${n}\``).join(", ");
      throw new ProgramError(`it imports \`${name}\`, which isn't one of the host's: ${all}`);
    }
    const ty = types.imports[i];
    if (imp.kind !== "function" || ty !== want) {
      const what = imp.kind === "function" ? `a function ${ty}` : `a ${imp.kind}`;
      throw new ProgramError(`its import \`${name}\` must be a function ${want}, not ${what}`);
    }
  });
    if (memory === null) {
    throw new ProgramError(`it doesn't import its memory, shared, as \`${IMPORT_MODULE}.${IMPORT_MEMORY}\``);
  }
  const exports = new Map(WebAssembly.Module.exports(module).map((e) => [e.name, e.kind]));
  if (exports.get(EXPORT_MEMORY) !== "memory") {
    throw new ProgramError(`it doesn't export its memory as \`${EXPORT_MEMORY}\``);
  }
  if (exports.get(EXPORT_FRAME) !== "function" || types.exports.get(EXPORT_FRAME) !== "(f32, i32, i32) -> ()") {
    throw new ProgramError(`it must export \`${EXPORT_FRAME}(time: f32, width: i32, height: i32)\` with no results`);
  }
  if (exports.has(EXPORT_INIT) && (exports.get(EXPORT_INIT) !== "function" || types.exports.get(EXPORT_INIT) !== "() -> ()")) {
    throw new ProgramError(`its \`${EXPORT_INIT}\` export must be a function with no parameters and no results`);
  }
  return memory;
}

/** A program's module, checked, and its memory's limits. */
export interface Compiled {
  module: WebAssembly.Module;
  memory: MemoryLimits;
}

/** Starts worker thread `index` on `module`, with `memory`: something that instantiates it
 * there and calls `__worker(index)` (`runWorker`). */
export type SpawnWorker = (module: WebAssembly.Module, memory: WebAssembly.Memory, index: number) => void;

/** On a worker thread: instantiates the program with the shared memory and runs jobs until the
 * program shuts it down. If it traps, it tells the thread waiting for the job (wrela_abi
 * memory's workers). */
export async function runWorker(module: WebAssembly.Module, memory: WebAssembly.Memory, index: number): Promise<void> {
  const refuse = () => {
    throw new TrapError("a worker can't call the host");
  };
  const imports = {
    [IMPORT_MODULE]: { [IMPORT_MEMORY]: memory, ...Object.fromEntries(HOST_FUNCTIONS.map(([name]) => [name, refuse])) },
  };
  try {
    const instance = await WebAssembly.instantiate(module, imports);
    (instance.exports[EXPORT_WORKER] as (i: number) => void)(index);
  } catch {
    const words = new Int32Array(memory.buffer);
    Atomics.store(words, WORKERS_FAILED / 4, 1);
    Atomics.or(words, WORKERS_DONE / 4, WORKERS_DONE_FAILED);
    Atomics.notify(words, WORKERS_DONE / 4);
  }
}

type FrameFn = (time: number, width: number, height: number) => void;

/** How a program is run. */
export interface ProgramOptions {
  /** Keep the state hash of every submitted byte (test mode reads it; it costs a pass over
   * each batch, so a game running normally doesn't keep it). */
  hash?: boolean;
  /** Where storage and fetch requests go; without it, they fail. */
  io?: Io;
  /** Plays the voice the program starts (`std::audio::play`), on an audio thread; without
   * it, the voice is kept but not played. */
  startVoice?: (voice: VoiceOptions) => void;
  /** How many threads run a parallel job's chunks, the program's own included: 1 (the
   * default) runs them all on it. More need `spawnWorker`. */
  workers?: number;
  spawnWorker?: SpawnWorker;
}

/** A loaded program and everything that happens to its batches. */
export class Program {
  /** The state hash, when the options ask for it. */
  readonly hash: StateHash | null;
  #sequencer = new Sequencer();
  #memory: WebAssembly.Memory | null = null;
  /** Where the code came from, for trap messages. */
  #lines: Lines | null = null;
  #frame: FrameFn | null = null;
  #instance: WebAssembly.Instance | null = null;
  #module: WebAssembly.Module | null = null;
  /** The voice the program started: its task and context. */
  #voice: { task: number; context: number } | null = null;
  readonly #startVoice: ((voice: VoiceOptions) => void) | null;
  /** The error behind the most recent trap raised by `submit`, so the caller gets it rather
   * than whatever the engine wraps it in. */
  #failure: unknown = null;
  /** The first error the executor gave: what it holds isn't known after it, so every later
   * command fails. (A command the sequencer or the checker rejects leaves no trace.) */
  #failed: string | null = null;
  readonly #requests = new Requests();
  readonly #io: Io | null;

  private constructor(
    readonly checker: Checker,
    readonly executor: Executor,
    options: ProgramOptions,
  ) {
    this.hash = options.hash ? new StateHash() : null;
    this.#io = options.io ?? null;
    this.#startVoice = options.startVoice ?? null;
  }

    static async load(wasm: Bytes, checker: Checker, executor: Executor, options: ProgramOptions = {}): Promise<Program> {
    return Program.instantiate(await Program.compile(wasm), checker, executor, options);
  }

  /** Compiles a program's WASM and checks it against the program ABI. */
  static async compile(wasm: Bytes): Promise<Compiled> {
    let module: WebAssembly.Module;
    try {
      module = await WebAssembly.compile(wasm);
    } catch (e) {
      throw new ProgramError(errorMessage(e));
    }
    return { module, memory: checkAbi(module, wasm) };
  }

  /** Instantiates a compiled program against the decoder, with its memory and its workers. */
  static async instantiate(
    compiled: Compiled,
    checker: Checker,
    executor: Executor,
    options: ProgramOptions = {},
  ): Promise<Program> {
    const { module } = compiled;
    let memory: WebAssembly.Memory;
    try {
      const { initial, maximum } = compiled.memory;
      memory = new WebAssembly.Memory({ initial, maximum: maximum ?? initial, shared: true });
    } catch (e) {
      throw new ProgramError(`can't make its memory: ${errorMessage(e)}`);
    }
    const program = new Program(checker, executor, options);
    try {
      program.#lines = Lines.of(module);
    } catch (e) {
      throw new ProgramError(`its \`${LINES_SECTION}\` section is malformed: ${errorMessage(e)}`);
    }
        const imports = {
      [IMPORT_MODULE]: {
        [IMPORT_MEMORY]: memory,
        [IMPORT_SUBMIT]: (ptr: number, len: number) => program.#submitImport(ptr, len),
        [IMPORT_REQUEST_STATUS]: (request: number) => program.#requestStatus(request >>> 0),
        [IMPORT_REQUEST_TAKE]: (request: number, ptr: number) => program.#requestTake(request >>> 0, ptr >>> 0),
        // One of the device's limits, a u32's bits; 0 past the last.
        [IMPORT_LIMIT]: (index: number) => (checker.limits[index >>> 0] ?? 0) | 0,
        [IMPORT_AUDIO]: (task: number, context: number) => program.#audioImport(task >>> 0, context >>> 0),
      },
    };
    // As the native host words it: a module that can't be instantiated isn't a valid program.
    let instance: WebAssembly.Instance;
    try {
      instance = await WebAssembly.instantiate(module, imports);
    } catch (e) {
      throw new ProgramError(errorMessage(e));
    }
    program.#instance = instance;
    program.#module = module;
    program.#memory = memory;
    program.#frame = instance.exports[EXPORT_FRAME] as FrameFn;
    // The workers: each its own instance of the module on its own thread, with the memory.
    if (EXPORT_WORKER in instance.exports && options.spawnWorker) {
      const n = Math.min(Math.max(options.workers ?? 1, 1), MAX_WORKERS + 1) - 1;
      for (let i = 0; i < n; i++) options.spawnWorker(module, memory, i);
    }
    // A program with state makes it once, before anything else runs (language.md §12).
    const init = instance.exports[EXPORT_INIT];
    if (typeof init === "function") program.#call(() => (init as () => void)());
    return program;
  }

  /** Decodes and runs one batch. The state hash covers every submitted byte, valid or not. */
  #submit(batch: Bytes): void {
    this.hash?.update(batch);
    for (const cmd of decode(batch)) {
      if (this.#failed !== null) {
        throw new CommandError(cmd.op, `the host failed to carry out an earlier command (${this.#failed})`);
      }
      const before = this.#sequencer.copy();
      this.#sequencer.step(cmd);
      try {
        this.checker.check(cmd);
      } catch (e) {
        this.#sequencer = before;
        throw e;
      }
      try {
        this.executor.execute(cmd);
      } catch (e) {
        this.#failed = errorMessage(e);
        throw e;
      }
      this.#request(cmd);
    }
  }

  /** Starts the request a command makes, if it makes one. */
  #request(cmd: Command): void {
    const io = this.#io;
    const none = (what: string) => () => Promise.reject(new Error(`this host has no ${what}`));
    switch (cmd.op) {
      case "ReadBuffer": {
        const { handle, offset, size } = cmd;
        const readBack = this.executor.readBack?.bind(this.executor);
        this.#requests.start(cmd.op, cmd.request, readBack ? () => readBack(handle, offset, size) : none("GPU to read back from"));
        return;
      }
      case "StorageRead":
        this.#requests.start(cmd.op, cmd.request, io ? () => io.storageRead(cmd.path) : none("storage"));
        return;
      case "StorageWrite": {
        // The data is a view into the program's memory: copy it.
        const data = cmd.data.slice();
        const write = io ? () => io.storageWrite(cmd.path, data).then(() => new Uint8Array(0)) : none("storage");
        this.#requests.start(cmd.op, cmd.request, write);
        return;
      }
      case "Fetch":
        this.#requests.start(cmd.op, cmd.request, io ? () => io.fetch(cmd.url) : none("build to fetch from"));
        return;
      case "Log":
        // `std::io::print`: a line for whoever runs the program, prefixed so it's found.
        console.log(`wrela: ${cmd.text}`);
        return;
      default:
        return;
    }
  }

  /** `wrela.request_status(request)`: -1 while it's pending, -2 if it failed, else its
   * answer's length in bytes. */
  #requestStatus(request: number): number {
    const a = this.#requests.answered.get(request);
    if (a === undefined) return REQUEST_PENDING;
    return "error" in a ? REQUEST_FAILED : a.bytes.length;
  }

  /** `wrela.request_take(request, ptr)`: copies an answered request's bytes to `ptr`, and
   * forgets the request (a failed one too). */
  #requestTake(request: number, ptr: number): void {
    this.#starting();
    const a = this.#requests.answered.get(request);
    if (a === undefined) throw new TrapError(`request_take(${request}): the request isn't answered`);
    this.#requests.answered.delete(request);
    const bytes = "bytes" in a ? a.bytes : new Uint8Array(0);
    const memory = new Uint8Array(this.#memory!.buffer);
    if (ptr + bytes.length > memory.length) {
      throw new TrapError(`request_take(${request}, ${ptr}) reaches past the end of the program's memory`);
    }
    memory.set(bytes, ptr);
  }

  #submitImport(ptr: number, len: number): void {
    this.#starting();
    try {
      // The import is (i32, i32): reinterpret as unsigned, as the native host does.
      const [p, n] = [ptr >>> 0, len >>> 0];
            const memory = new Uint8Array(this.#memory!.buffer);
      if (p + n > memory.length) {
        throw new TrapError(
          `submit(${p}, ${n}) reaches past the end of the program's memory (${memory.length} bytes)`,
        );
      }
      // Not copied (D-097): WebGPU reads the shared memory itself, and nothing runs in it until
      // this call returns (the program's thread is here, and no parallel job runs across a call
      // to the host). What outlives the batch is copied (`Bytes`).
      this.#submit(new Uint8Array(this.#memory!.buffer, p, n) as Bytes);
    } catch (e) {
      this.#failure = e;
      throw e;
    }
  }

  /** `wrela.audio(task, context)`: starts the program's voice, once. */
  #audioImport(task: number, context: number): void {
    this.#starting();
    if (this.#voice !== null) throw new TrapError("a program starts one voice, and this one started a second");
    if (!(EXPORT_AUDIO in this.#instance!.exports)) throw new TrapError(`it starts a voice but doesn't export \`${EXPORT_AUDIO}\``);
    this.#voice = { task, context };
    this.#startVoice?.({ module: this.#module!, memory: this.#memory!, task, context });
  }

  /** Whether the program has started its voice. */
  get hasVoice(): boolean {
    return this.#voice !== null;
  }

  /** Refuses a call from the module's start function, which runs while it's instantiated (the
   * compiler's copies the constants into the memory, wrela_abi memory's `DATA_READY`). */
  #starting(): void {
    if (this.#instance === null) {
      throw new Error("its start function called the host; a wrela program calls the host only from an export");
    }
  }

  /** Calls `frame(time, width, height)`, then ends the frame. */
  frame(time: number, width: number, height: number): void {
    this.#call(() => this.#frame!(time, width, height));
    this.#sequencer.endFrame();
    this.executor.flush();
  }

  /** How many chunks of parallel jobs the workers (not the program's own thread) have run. */
  workerChunks(): number {
    if (!this.#memory) return 0;
    return Atomics.load(new Uint32Array(this.#memory.buffer), WORKERS_HELPED / 4);
  }

  /** Calls another export (for tests). */
  call(name: string, ...args: number[]): unknown {
    const f = this.#instance?.exports[name];
    if (typeof f !== "function") throw new ProgramError(`it has no exported function \`${name}\``);
    return this.#call(() => (f as (...a: number[]) => unknown)(...args));
  }

  /** The message a panic left before it trapped, cleared so a later trap that isn't a panic
   * doesn't repeat it. */
  #takePanicMessage(): string | null {
    if (!this.#memory) return null;
    const view = new DataView(this.#memory.buffer);
    const count = Math.min(view.getUint32(PANIC_MESSAGE, true), PANIC_CAP);
    if (count === 0) return null;
        // A copy: TextDecoder doesn't read shared memory.
    const bytes = new Uint8Array(this.#memory.buffer, PANIC_MESSAGE + 4, count).slice();
    const msg = new TextDecoder().decode(bytes);
    view.setUint32(PANIC_MESSAGE, 0, true);
    return msg;
  }

  /** What trapped, and where in the source if the program says and the engine's stack trace
   * gives offsets: the location of the innermost frame that has one. */
  #describe(e: unknown): string {
    const what = errorMessage(e);
    const stack = e instanceof Error ? (e.stack ?? "") : "";
    const at = wasmOffsets(stack)
      .map((offset) => this.#lines?.at(offset) ?? null)
      .find((loc) => loc !== null);
    return at ? `${what} at ${at}` : what;
  }

  #call<T>(f: () => T): T {
    this.#requests.settle();
    this.#failure = null;
    let result: T;
    try {
      result = f();
    } catch (e) {
      const failure = this.#failure;
      this.#failure = null;
      if (failure !== null) throw failure;
      const msg = this.#takePanicMessage();
      const what = this.#describe(e);
      throw new TrapError(msg === null ? what : `panic: ${msg}: ${what}`);
    }
    // A module can catch the exception a failed submit throws (WASM exception handling) and
    // return as if nothing happened: the call fails anyway.
    const failure = this.#failure;
    this.#failure = null;
    if (failure !== null) throw failure;
    return result;
  }
}
