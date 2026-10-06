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
  EXPORT_TICK,
  EXPORT_WORKER,
  HOST_FUNCTIONS,
  IMPORT_MEMORY,
  JOB_DONE,
  JOB_DONE_FAILED,
  JOB_FAILED,
  JOB_SLOT_COUNT,
  JOB_SLOT_SIZE,
  JOB_SLOTS,
  MAX_WORKERS,
  PAR_HELPED,
  RUNNING,
  SLOT_FAILED,
  SLOT_STATE,
  SLOT_THREAD,
  THREAD_BLOCK_SIZE,
  THREAD_BLOCKS,
  THREAD_HELPER0,
  THREAD_MAIN,
  THREADS,
  EVENT_SIZE,
  IMPORT_AUDIO,
  IMPORT_INPUT,
  IMPORT_LIMIT,
  IMPORT_MODULE,
  IMPORT_REQUEST_STATUS,
  IMPORT_REQUEST_TAKE,
  IMPORT_SUBMIT,
  IMPORT_TICK,
  PANIC,
  TICK_WANT_HASH,
  PANIC_CAP,
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

/** Where a program's requests read and write: its storage (bytes at paths) and its build, and
 * where its posts go: the host that serves the build. */
export interface Io {
  storageRead(path: string): Promise<Bytes>;
  storageWrite(path: string, data: Bytes): Promise<void>;
  fetch(url: string): Promise<Bytes>;
  post(url: string, body: Bytes): Promise<Bytes>;
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

/** Starts helper thread `thread` (its number, wrela_abi memory's threads) on `module`, with
 * `memory`: something that instantiates it there and calls `__worker(thread)` (`runWorker`). */
export type SpawnWorker = (module: WebAssembly.Module, memory: WebAssembly.Memory, thread: number) => void;

/** Another thread's instance of the program: its code records no GPU work and makes no
 * requests (its effects are checked), so the host's functions refuse to run, as `who`. */
export function otherInstance(module: WebAssembly.Module, memory: WebAssembly.Memory, who: string): Promise<WebAssembly.Instance> {
  const refuse = () => {
    throw new TrapError(`${who} can't call the host`);
  };
  const imports = {
    [IMPORT_MODULE]: { [IMPORT_MEMORY]: memory, ...Object.fromEntries(HOST_FUNCTIONS.map(([name]) => [name, refuse])) },
  };
  return WebAssembly.instantiate(module, imports);
}

/** On a helper thread, number `thread`: instantiates the program with the shared memory and
 * runs work until the program shuts it down. If it traps, it tells the thread that waits for
 * what it ran (wrela_abi memory's helpers), and runs nothing more. */
export async function runWorker(module: WebAssembly.Module, memory: WebAssembly.Memory, thread: number): Promise<void> {
  try {
    const instance = await otherInstance(module, memory, "a helper");
    (instance.exports[EXPORT_WORKER] as (thread: number) => void)(thread);
  } catch {
    helperTrapped(memory, thread);
  }
}

/** A helper, `thread`, trapped: the thread waiting for what it ran learns it, and traps with its
 * panic message (wrela_abi memory's helpers), as the native host does. */
export function helperTrapped(memory: WebAssembly.Memory, thread: number): void {
  const words = new Int32Array(memory.buffer);
  const running = Atomics.load(words, (THREAD_BLOCKS + thread * THREAD_BLOCK_SIZE + RUNNING) / 4) >>> 0;
  if (running >= THREAD_BLOCKS && running < THREAD_BLOCKS + THREADS * THREAD_BLOCK_SIZE) {
    // A thread's parallel job: its starting thread waits on its count of done chunks.
    Atomics.store(words, (running + JOB_FAILED) / 4, thread + 1);
    Atomics.or(words, (running + JOB_DONE) / 4, JOB_DONE_FAILED | 0);
    Atomics.notify(words, (running + JOB_DONE) / 4);
  } else if (running >= JOB_SLOTS && running < JOB_SLOTS + JOB_SLOT_COUNT * JOB_SLOT_SIZE) {
    // A long job: whoever joins it waits on its state.
    Atomics.store(words, (running + SLOT_THREAD) / 4, thread);
    Atomics.store(words, (running + SLOT_STATE) / 4, SLOT_FAILED);
    Atomics.notify(words, (running + SLOT_STATE) / 4);
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
  /** Called with each line the program prints (`std::io::print`), after the console has it:
   * test mode keeps them (`results/log.txt`). */
  onPrint?: (line: string) => void;
  /** Starts the program's ticker (`std::tick::start`) on its own thread; without it, the ticker
   * is kept but never ticks. */
  startTicker?: (ticker: TickerOptions) => void;
  /** Each tick reports its state's hash (wrela_abi memory's `TICK_WANT_HASH`), and so does the
   * ticker's `start`, in `init`: test mode keeps them in a tick log. */
  tickHashes?: boolean;
}

/** The program's ticker, as its thread gets it: the module and memory to run it with, and the
 * task, context and rate `__tick` takes (wrela_abi's `IMPORT_TICK`). */
export interface TickerOptions {
  module: WebAssembly.Module;
  memory: WebAssembly.Memory;
  task: number;
  context: number;
  hz: number;
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
  /** The ticker the program started: its task, context and rate. */
  #ticker: { task: number; context: number; hz: number } | null = null;
  readonly #startTicker: ((ticker: TickerOptions) => void) | null;
  /** The error behind the most recent trap raised by `submit`, so the caller gets it rather
   * than whatever the engine wraps it in. */
  #failure: unknown = null;
  /** The first error the executor gave: what it holds isn't known after it, so every later
   * command fails. (A command the sequencer or the checker rejects leaves no trace.) */
  #failed: string | null = null;
  readonly #requests = new Requests();
  readonly #io: Io | null;
  readonly #onPrint: ((line: string) => void) | null;
  /** Input events not yet read (`wrela.input`), EVENT_SIZE bytes each, oldest first. */
  #input: Uint8Array[] = [];

  private constructor(
    readonly checker: Checker,
    readonly executor: Executor,
    options: ProgramOptions,
  ) {
    this.hash = options.hash ? new StateHash() : null;
    this.#io = options.io ?? null;
    this.#startVoice = options.startVoice ?? null;
    this.#startTicker = options.startTicker ?? null;
    this.#onPrint = options.onPrint ?? null;
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
        [IMPORT_INPUT]: (ptr: number, cap: number) => program.#inputImport(ptr >>> 0, cap >>> 0),
        [IMPORT_TICK]: (task: number, context: number, hz: number) => program.#tickImport(task >>> 0, context >>> 0, hz >>> 0),
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
    // The helpers: each its own instance of the module on its own thread, with the memory.
    if (EXPORT_WORKER in instance.exports && options.spawnWorker) {
      const n = Math.min(Math.max(options.workers ?? 1, 1), MAX_WORKERS + 1) - 1;
      for (let i = 0; i < n; i++) options.spawnWorker(module, memory, THREAD_HELPER0 + i);
    }
    if (options.tickHashes) new DataView(memory.buffer).setUint32(TICK_WANT_HASH, 1, true);
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
      case "Post": {
        // The body is a view into the program's memory: copy it.
        const body = cmd.body.slice();
        this.#requests.start(cmd.op, cmd.request, io ? () => io.post(cmd.url, body) : none("server to post to"));
        return;
      }
      case "Log":
        // `std::io::print`: a line for whoever runs the program, prefixed so it's found.
        console.log(`wrela: ${cmd.text}`);
        this.#onPrint?.(cmd.text);
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

  /** Queues input events (EVENT_SIZE bytes each): the program reads them from its next call
   * on (runtime/abi `input`). */
  queueInput(events: Uint8Array): void {
    for (let at = 0; at + EVENT_SIZE <= events.length; at += EVENT_SIZE) {
      this.#input.push(events.slice(at, at + EVENT_SIZE));
    }
  }

  /** `wrela.input(ptr, cap) -> count`: copies up to `cap` queued events to `ptr`. */
  #inputImport(ptr: number, cap: number): number {
    this.#starting();
    const n = Math.min(cap, this.#input.length);
    const memory = new Uint8Array(this.#memory!.buffer);
    if (ptr + n * EVENT_SIZE > memory.length) {
      throw new TrapError(`input(${ptr}, ${cap}) reaches past the end of the program's memory`);
    }
    for (let i = 0; i < n; i++) memory.set(this.#input[i]!, ptr + i * EVENT_SIZE);
    this.#input.splice(0, n);
    return n;
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

  /** `wrela.tick(task, context, hz)`: starts the program's ticker, once: a second is its panic. */
  #tickImport(task: number, context: number, hz: number): void {
    this.#starting();
    if (this.#ticker !== null) throw new TrapError("panic: a program starts one ticker, and this one started a second");
    if (!(EXPORT_TICK in this.#instance!.exports)) throw new TrapError(`it starts a ticker but doesn't export \`${EXPORT_TICK}\``);
    this.#ticker = { task, context, hz };
    this.#startTicker?.({ module: this.#module!, memory: this.#memory!, task, context, hz });
  }

  /** The ticker the program started, if it has: its task, context and rate. */
  get ticker(): { task: number; context: number; hz: number } | null {
    return this.#ticker;
  }

  /** The program's module and memory, for another thread's instance. */
  get shared(): { module: WebAssembly.Module; memory: WebAssembly.Memory } {
    return { module: this.#module!, memory: this.#memory! };
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
    return Atomics.load(new Uint32Array(this.#memory.buffer), PAR_HELPED / 4);
  }

  /** Calls another export (for tests). */
  call(name: string, ...args: number[]): unknown {
    const f = this.#instance?.exports[name];
    if (typeof f !== "function") throw new ProgramError(`it has no exported function \`${name}\``);
    return this.#call(() => (f as (...a: number[]) => unknown)(...args));
  }

  /** The message a panic on the program's thread left before it trapped, cleared so a later trap
   * that isn't a panic doesn't repeat it. */
  #takePanicMessage(): string | null {
    if (!this.#memory) return null;
    return takePanicMessage(this.#memory, THREAD_MAIN);
  }

  #describe(e: unknown): string {
    return describeTrap(e, this.#lines);
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

/** What trapped, and where in the source if the program says (`lines`) and the engine's stack
 * trace gives offsets: the location of the innermost frame that has one. */
export function describeTrap(e: unknown, lines: Lines | null): string {
  const what = errorMessage(e);
  const stack = e instanceof Error ? (e.stack ?? "") : "";
  const at = wasmOffsets(stack)
    .map((offset) => lines?.at(offset) ?? null)
    .find((loc) => loc !== null);
  return at ? `${what} at ${at}` : what;
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
