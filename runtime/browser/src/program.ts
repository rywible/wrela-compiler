// The CPU side: a program's WASM, its imports, the batches it submits, and its requests.
// Mirrors runtime/native/src/program.rs: the same ABI checks before instantiating, then hash,
// decode, sequence and check each batch; execute only after the export succeeds. A request is
// answered when its answer arrives (a readback, storage, a fetch: all asynchronous here), and
// the program sees the answer from its next call on (language.md §17.1).

import {
  AUDIO_CHANNELS,
  AUDIO_OUT,
  AUDIO_QUANTUM,
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
  TASK_SLOTS,
  TASK_SLOT_SIZE,
  TASK_SLOTS_END,
  MAX_WORKERS,
  MAX_TICK_RECORDS,
  PAR_HELPED,
  PAR_HELPERS,
  PAR_SHUTDOWN,
  PAR_WAKE,
  RUNNING,
  SLOT_FAILED,
  SLOT_STATE,
  SLOT_THREAD,
  THREAD_BLOCK_SIZE,
  THREAD_BLOCKS,
  THREAD_BLOCKS_END,
  THREAD_HELPER0,
  THREAD_MAIN,
  EVENT_SIZE,
  EXPORT_LIFT_SET,
  IMPORT_AUDIO,
  IMPORT_INPUT,
  IMPORT_PAYLOAD,
  PAIR_SECTION,
  IMPORT_KEEP,
  IMPORT_KEPT,
  IMPORT_CLOCK,
  IMPORT_PHASE,
  IMPORT_PRINT,
  IMPORT_LIMIT,
  IMPORT_MODULE,
  IMPORT_REQUEST_STATUS,
  IMPORT_REQUEST_TAKE,
  IMPORT_SUBMIT,
  IMPORT_TICK,
  PANIC,
  TICK_WANT_HASH,
  TICK_RECORDS,
  PANIC_CAP,
  REQUEST_FAILED,
  REQUEST_MISSING,
  REQUEST_PENDING,
} from "./abi.gen.ts";
import { type Checker, checkHostRules, CommandError } from "./check.ts";
import { errorMessage } from "./errors.ts";
import { StateHash } from "./hash.ts";
import { CALLED_WHILE_STARTING, clockImport, phaseImport, Phases, printed, refusingImports } from "./imports.ts";
import { Lines, LINES_SECTION, unlocated, wasmOffsets } from "./lines.ts";
import { Inbox, type Net } from "./net.ts";
import type { Scripted } from "./input.ts";
import { type Bytes, type Command, decode, type OpcodeName, Sequencer } from "./stream.ts";
import type { TickerOptions, VoiceOptions } from "./messages.ts";
import type { MemoryLimits, WasmAbi } from "./manifest.ts";
import { wasmHash } from "./ticks.ts";
import { UTF8 } from "./wasm.ts";

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
  /** Called after each successful export: submit its work. */
  flush(): void;
  /** A buffer's bytes, for a `ReadBuffer` request, once the GPU's work recorded so far is
   * done. An executor without a GPU doesn't have it. */
  readBack?(handle: number, offset: number, size: number): Promise<Bytes>;
  /** The GPU time of the work labelled `label` from this frame on, for a `Timed` request: an
   * `f32` of microseconds, once read back. An executor without a GPU doesn't have it. */
  timeLabel?(label: string): Promise<Bytes>;
}

/** Where a program's requests read and write: its storage (bytes at paths) and its build, and
 * where its posts go: the host that serves the build. */
export interface Io {
  /** The bytes stored at `path`, or null if nothing is stored there. */
  storageRead(path: string): Promise<Bytes | null>;
  storageWrite(path: string, data: Bytes): Promise<void>;
  fetch(url: string): Promise<Bytes>;
  post(url: string, body: Bytes): Promise<Bytes>;
  hostRequest?(service: string, body: Bytes): Promise<Bytes>;
}

/** The other builds a program may hand its place to (`std::reload`), each named by its path from
 * the program's own build. */
export interface Builds {
  /** Gets the build ready: fetched, compiled, and its pipelines built. */
  prepare(path: string): Promise<void>;
  /** Lets go of what was got ready for the build. */
  unprepare(path: string): void;
  /** Hands the program's place to the build after the call that asked. Rejected, with why, if
   * the build can't be got ready, and the program goes on; never resolved if the hand-over is
   * made, since the program stops. */
  handOver(path: string): Promise<void>;
}

/** An answer: the bytes, or why the request failed, and whether that was because a storage read's
 * path holds nothing. */
type Answer = { bytes: Bytes } | { error: string; missing?: true };

/** The program's requests: in flight, arrived during or between calls (seen from the next
 * call), and answered but not taken. */
class Requests {
  readonly #inFlight = new Set<number>();
  readonly #arriving: [number, Answer][] = [];
  readonly #reserved = new Set<number>();
  readonly answered = new Map<number, Answer>();

  #check(op: OpcodeName, request: number): void {
    if (this.#inFlight.has(request) || this.answered.has(request) || this.#arriving.some((a) => a[0] === request)) {
      throw new CommandError(op, `request ${request} is already in use`);
    }
  }

  /** Checks an export's request now, before any of its effects can reach the host. */
  reserve(op: OpcodeName, request: number): void {
    this.#check(op, request);
    if (this.#reserved.has(request)) throw new CommandError(op, `request ${request} is already in use`);
    this.#reserved.add(request);
  }

  endCall(): void {
    this.#reserved.clear();
  }

  /** Starts request `request`, whose answer `work` gives: null for a storage read of nothing. */
  start(op: OpcodeName, request: number, work: () => Promise<Bytes | null>): void {
    this.#check(op, request);
    this.#inFlight.add(request);
    let promise: Promise<Bytes | null>;
    try {
      promise = work();
    } catch (e) {
      promise = Promise.reject(e);
    }
    promise.then(
      (bytes) => this.#arrive(request, bytes === null ? { error: "nothing is stored there", missing: true } : { bytes }),
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

/** What a host without `Builds` has none of (as the native host words it). */
const OTHER_BUILD = "other build to hand over to";

/** The imports a program may have, with their types as both hosts word them. */
const IMPORTS = new Map<string, string>(HOST_FUNCTIONS);

/**
 * Checks a module against the program ABI before instantiating it: it imports its memory,
 * shared, as `wrela.memory`, may import only the host's functions (`IMPORTS`) besides, must
 * export `memory` and `frame(f32, i32, i32)`. (It may have a start function, which may not call
 * the host: `#starting`.) What JavaScript can't see of the module, its functions' types and its
 * memory's limits, comes from `abi`, its build's manifest's: whose names and kinds must be the
 * module's own. The memory's limits.
 */
export function checkAbi(module: WebAssembly.Module, abi: WasmAbi | null): MemoryLimits {
  if (abi === null) throw new ProgramError("its manifest doesn't state its WASM's interface (`wasm_abi`)");
  const imported = WebAssembly.Module.imports(module);
  const exported = WebAssembly.Module.exports(module).filter((e) => e.kind === "function");
  const kindOf = (ty: string) => (ty.startsWith("(") ? "function" : ty);
  const same =
    imported.length === abi.imports.length &&
    imported.every((imp, i) => {
      const [m, n, ty] = abi.imports[i]!;
      return imp.module === m && imp.name === n && imp.kind === kindOf(ty);
    }) &&
    exported.length === abi.exports.length &&
    exported.every((e, i) => e.name === abi.exports[i]![0]);
  if (!same) throw new ProgramError("its manifest's `wasm_abi` doesn't describe its WASM");
  const types = {
    imports: abi.imports.map(([, , ty]) => (ty.startsWith("(") ? ty : null)),
    exports: new Map(abi.exports.map(([name, ty]) => [name, ty])),
    memories: new Map<number, MemoryLimits>(),
  };
  imported.forEach((imp, i) => {
    if (imp.module === IMPORT_MODULE && imp.name === IMPORT_MEMORY && abi.memory !== null) types.memories.set(i, abi.memory);
  });
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
  const threadEntries = [
    [EXPORT_WORKER, "(i32) -> ()"],
    [EXPORT_AUDIO, "(i32, i32, i32) -> ()"],
    [EXPORT_TICK, "(i32, i32, i32, i32) -> ()"],
  ] as const;
  for (const [name, want] of threadEntries) {
    if (exports.has(name) && (exports.get(name) !== "function" || types.exports.get(name) !== want)) {
      throw new ProgramError(`its \`${name}\` export must be a function ${want}`);
    }
  }
  return memory;
}

/** A program's module, checked, its memory's limits, and how it was compiled. */
export interface Compiled {
  module: WebAssembly.Module;
  memory: MemoryLimits;
  /** The module's bytes' hash (`ticks.ts`), when it was asked for. */
  hash: bigint | null;
  /** How long the compile took (ms), and whether it streamed. */
  compile_ms: number;
  streamed: boolean;
  /** How long its first instantiation took, with its `init` (ms), once it has been. */
  instantiate_ms?: number;
}

/** A line printed by any of a program's threads, after that thread has shown it on its console. */
export type PrintSink = (line: string) => void;

/** Starts helper thread `thread` (its number, wrela_abi memory's threads) on `module`, with
 * `memory`: something that instantiates it there and calls `__worker(thread)` (`runWorker`). */
export type SpawnWorker = (
  module: WebAssembly.Module,
  memory: WebAssembly.Memory,
  thread: number,
  onPrint: PrintSink,
) => void;

/** Another thread's instance of the program, `who`'s: the host's functions trap (`refusingImports`). */
export function otherInstance(
  module: WebAssembly.Module,
  memory: WebAssembly.Memory,
  who: string,
  onPrint: PrintSink = () => {},
): Promise<WebAssembly.Instance> {
  let started = false;
  let hostFailure: Error | null = null;
  return WebAssembly.instantiate(
    module,
    refusingImports(memory, who, TrapError, onPrint, () => started, (error) => (hostFailure = error)),
  ).then((instance) => {
    if (hostFailure !== null) throw hostFailure;
    started = true;
    // A Wasm catch_all can catch an import's JavaScript exception. Check each thread entry
    // after it returns too, so it cannot turn a rejected host call into a successful tick/job.
    const exports = Object.fromEntries(Object.entries(instance.exports).map(([name, value]) => [name, typeof value !== "function" ? value : (...args: number[]) => {
      if (hostFailure !== null) throw hostFailure;
      let result: unknown;
      try { result = value(...args); } catch (e) { throw hostFailure ?? e; }
      if (hostFailure !== null) throw hostFailure;
      return result;
    }]));
    return { exports };
  });
}

/** On a helper thread, number `thread`: instantiates the program with the shared memory and
 * runs work until the program shuts it down. If it traps, it tells the thread that waits for
 * what it ran (wrela_abi memory's helpers), and runs nothing more. */
export async function runWorker(
  module: WebAssembly.Module,
  memory: WebAssembly.Memory,
  thread: number,
  onPrint: PrintSink = () => {},
): Promise<void> {
  try {
    const instance = await otherInstance(module, memory, "a helper", onPrint);
    (instance.exports[EXPORT_WORKER] as (thread: number) => void)(thread);
  } catch (e) {
    // The program reports the trap where it's joined; a trap outside a job or a task would
    // otherwise go unseen.
    console.error(`a helper (thread ${thread}) trapped: ${describeTrap(e, Lines.of(module))}`);
    helperTrapped(memory, thread);
  }
}

/** The address of thread `thread`'s block (wrela_abi memory's `thread_block`). */
const threadBlock = (thread: number) => THREAD_BLOCKS + thread * THREAD_BLOCK_SIZE;

/** A helper, `thread`, trapped: the thread waiting for what it ran learns it, and traps with its
 * panic message (wrela_abi memory's helpers), as the native host does. */
function helperTrapped(memory: WebAssembly.Memory, thread: number): void {
  const at = threadBlock(thread) + RUNNING;
  if (at + 4 > memory.buffer.byteLength) return;
  const words = new Int32Array(memory.buffer);
  const running = Atomics.load(words, at / 4) >>> 0;
  if (running >= THREAD_BLOCKS && running < THREAD_BLOCKS_END &&
      (running - THREAD_BLOCKS) % THREAD_BLOCK_SIZE === 0 && running + JOB_FAILED + 4 <= memory.buffer.byteLength) {
    // A thread's parallel job: its starting thread waits on its count of done chunks. The first
    // helper to trap is the one whose message the program reports.
    Atomics.compareExchange(words, (running + JOB_FAILED) / 4, 0, thread + 1);
    Atomics.or(words, (running + JOB_DONE) / 4, JOB_DONE_FAILED | 0);
    Atomics.notify(words, (running + JOB_DONE) / 4);
  } else if (running >= TASK_SLOTS && running < TASK_SLOTS_END &&
      (running - TASK_SLOTS) % TASK_SLOT_SIZE === 0 && running + SLOT_THREAD + 4 <= memory.buffer.byteLength) {
    // A task: whoever joins it waits on its state.
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
  /** Check each command against WebGPU's rules before the executor runs it (the default), so
   * an error names the program's mistake: test mode, debug builds and hot reload. A release
   * build's normal run doesn't: WebGPU validates, and its `uncapturederror` is fatal. The
   * sequencer (a pass's commands inside it) and the rules WebGPU doesn't know (`checkHostRules`:
   * a request's path stays in its storage, build or origin) check either way. */
  checking?: boolean;
  /** Time its own thread's phases (`std::time::phase`) for `takePhases`: test mode takes them
   * after each frame. A normal run doesn't keep them, so names made without end cost nothing. */
  phases?: boolean;
  /** Where storage and fetch requests go; without it, they fail. */
  io?: Io;
  /** The builds the program may hand over to; without it, those requests fail. */
  builds?: Builds | undefined;
  /** Plays the voice the program starts (`std::audio::play`), on an audio thread; without
   * it, the voice is kept but not played. */
  startVoice?: (voice: VoiceOptions, onPrint: PrintSink) => void;
  /** How many threads run a parallel job's chunks, the program's own included: 1 (the
   * default) runs them all on it. More need `spawnWorker`. */
  workers?: number;
  spawnWorker?: SpawnWorker;
  /** Called with each line the program prints (`std::io::print`), after the console has it:
   * test mode keeps them (`results/log.txt`). */
  onPrint?: (line: string) => void;
  /** Starts the program's ticker (`std::tick::start`) on its own thread; without it, the ticker
   * is kept but never ticks. */
  startTicker?: (ticker: TickerOptions, onPrint: PrintSink) => void;
  /** Each tick reports its state's hash (wrela_abi memory's `TICK_WANT_HASH`), and so does the
   * ticker's `start`, in `init`: test mode keeps them in a tick log. */
  tickHashes?: boolean;
  /** Hot reload: what the build this one replaces kept (`wrela.kept`), and its input not yet
   * read. */
  kept?: Uint8Array | undefined;
  inbox?: Inbox | undefined;
  /** Its connections (`std::net`): without them, `net/` requests fail as any request this
   * host can't answer. */
  net?: Pick<Net, "open" | "send" | "close" | "closeAll" | "plan"> | undefined;
}

/** The pair id in a module's PAIR_SECTION, if it has one. */
export function pairOf(module: WebAssembly.Module): bigint | null {
  const [section] = WebAssembly.Module.customSections(module, PAIR_SECTION);
  if (section === undefined || section.byteLength !== 8) return null;
  return new DataView(section).getBigUint64(0, true);
}

/** A connection's number: the first word of a `net/` request's body. */
function connOf(service: string, body: Bytes): number {
  if (body.length < 4) throw new TrapError(`\`${service}\` needs a connection's number`);
  return new DataView(body.buffer, body.byteOffset, 4).getUint32(0, true);
}

/** A loaded program and everything that happens to its batches. */
export class Program {
  /** The state hash, when the options ask for it. */
  readonly hash: StateHash | null;
  #sequencer = new Sequencer();
  /** Validated commands wait for the export to return successfully before execution. */
  #pending: Command[] = [];
  /** The phases its own thread times (`std::time::phase`), when the options ask for them. */
  readonly #phases: Phases | null;
  readonly #checking: boolean;
  #memory: WebAssembly.Memory | null = null;
  /** The memory's maximum, in bytes: what the program reserves. */
  #reserved = 0;
  /** Where the code came from, for trap messages. */
  #lines: Lines | null = null;
  #frame: FrameFn | null = null;
  #instance: WebAssembly.Instance | null = null;
  #module: WebAssembly.Module | null = null;
  /** Initialization has finished: thread callbacks stay local until `init` succeeds. */
  #ready = false;
  /** The voice the program started: its task and context. */
  #voice: { task: number; context: number } | null = null;
  readonly #startVoice: ((voice: VoiceOptions, onPrint: PrintSink) => void) | null;
  /** The ticker the program started: its task, context and rate. */
  #ticker: Pick<TickerOptions, "task" | "context" | "hz"> | null = null;
  readonly #startTicker: ((ticker: TickerOptions, onPrint: PrintSink) => void) | null;
  /** The first trap raised by a host import in this call, so the caller gets it rather
   * than whatever the engine wraps it in. */
  #failure: unknown = null;
  /** The first error while committing host work: what it holds isn't known after it, so every later
   * command fails. (A command the sequencer or the checker rejects leaves no trace.) */
  #failed: string | null = null;
  readonly #requests = new Requests();
  readonly #io: Io | null;
  readonly #builds: Builds | null;
  readonly #onPrint: PrintSink | null;
  /** Input events not yet read (`wrela.input`), with messages' bytes. */
  readonly #inbox: Inbox;
  readonly #net: ProgramOptions["net"] | null;
  /** This export's planned connection states, made only when it submits a network command. */
  #netPlan: Pick<Net, "open" | "send" | "close"> | null = null;
  /** Its build's pair id, which its connections give its server. */
  #pair: bigint | null = null;
  /** What the replaced build kept (`wrela.kept`), and what this one keeps (`wrela.keep`). */
  readonly #kept: Uint8Array;
  #keeping = new Uint8Array(0);

  private constructor(
    readonly checker: Checker,
    readonly executor: Executor,
    options: ProgramOptions,
  ) {
    this.hash = options.hash ? new StateHash() : null;
    this.#phases = options.phases ? new Phases() : null;
    this.#checking = options.checking ?? true;
    this.#io = options.io ?? null;
    this.#builds = options.builds ?? null;
    this.#startVoice = options.startVoice ?? null;
    this.#startTicker = options.startTicker ?? null;
    this.#onPrint = options.onPrint ?? null;
    this.#kept = options.kept ?? new Uint8Array(0);
    this.#inbox = options.inbox ?? new Inbox();
    this.#net = options.net ?? null;
  }

  static async load(wasm: Bytes, abi: WasmAbi | null, checker: Checker, executor: Executor, options: ProgramOptions = {}): Promise<Program> {
    return Program.instantiate(await Program.compile(wasm, abi), checker, executor, options);
  }

  /** Compiles a program's WASM and checks it against the program ABI, which its build's
   * manifest states (`abi`). A response the server says is WASM compiles as it streams in (which
   * the browser's code cache can serve on a later open); anything else is read whole first.
   * With `hash`, the module's bytes are hashed too (the tick log's header). */
  static async compile(wasm: Response | Bytes, abi: WasmAbi | null, hash = false): Promise<Compiled> {
    let module: WebAssembly.Module;
    let hashed: Promise<bigint | null> = Promise.resolve(null);
    const started = performance.now();
    let streamed = false;
    try {
      if (wasm instanceof Response) {
        if (hash) hashed = wasm.clone().arrayBuffer().then((b) => wasmHash(new Uint8Array(b)));
        streamed = (wasm.headers.get("Content-Type") ?? "").split(";")[0]!.trim() === "application/wasm";
        module = streamed ? await WebAssembly.compileStreaming(wasm) : await WebAssembly.compile(await wasm.arrayBuffer());
      } else {
        if (hash) hashed = Promise.resolve(wasmHash(wasm));
        module = await WebAssembly.compile(wasm);
      }
    } catch (e) {
      throw new ProgramError(errorMessage(e));
    }
    const compile_ms = performance.now() - started;
    return { module, memory: checkAbi(module, abi), hash: await hashed, compile_ms, streamed };
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
    // Network events queued for another program's build go, and so do its connections.
    program.#pair = pairOf(module);
    if (program.#inbox.forBuild(program.#pair)) program.#net?.closeAll();
    const clock = clockImport(memory);
    const phase = phaseImport(memory);
    try {
      program.#lines = Lines.of(module);
    } catch (e) {
      throw new ProgramError(`its \`${LINES_SECTION}\` section is malformed: ${errorMessage(e)}`);
    }
    const imports: WebAssembly.Imports = {
      [IMPORT_MODULE]: {
        [IMPORT_MEMORY]: memory,
        [IMPORT_SUBMIT]: (ptr: number, len: number) => program.#submitImport(ptr, len),
        [IMPORT_REQUEST_STATUS]: (request: number) => program.#requestStatus(request >>> 0),
        [IMPORT_REQUEST_TAKE]: (request: number, ptr: number) => program.#requestTake(request >>> 0, ptr >>> 0),
        // One of the device's limits, a u32's bits; 0 past the last.
        [IMPORT_LIMIT]: (index: number) => {
          program.#starting();
          return (checker.limits[index >>> 0] ?? 0) | 0;
        },
        [IMPORT_AUDIO]: (task: number, context: number) => program.#audioImport(task >>> 0, context >>> 0),
        [IMPORT_INPUT]: (ptr: number, cap: number) => program.#inputImport(ptr >>> 0, cap >>> 0),
        [IMPORT_PAYLOAD]: (lo: number, hi: number, ptr: number, cap: number) => program.#payloadImport(lo >>> 0, hi >>> 0, ptr >>> 0, cap >>> 0),
        [IMPORT_TICK]: (task: number, context: number, hz: number) => program.#tickImport(task >>> 0, context >>> 0, hz >>> 0),
        [IMPORT_KEEP]: (ptr: number, len: number) => program.#keepImport(ptr >>> 0, len >>> 0),
        [IMPORT_KEPT]: (ptr: number, cap: number) => program.#keptImport(ptr >>> 0, cap >>> 0),
        [IMPORT_CLOCK]: (ptr: number) => {
          program.#starting();
          clock(ptr);
        },
        // `std::io::print`: a line for whoever runs the program, shown as it's made.
        [IMPORT_PRINT]: (ptr: number, len: number) => {
          program.#starting();
          const line = printed(memory, ptr, len);
          program.#printed(line);
        },
        [IMPORT_PHASE]: (ptr: number, len: number) => {
          program.#starting();
          // The name is read and checked whether or not phases are timed.
          const name = phase(ptr, len);
          program.#phases?.begin(name);
        },
      },
    };
    // JavaScript exceptions thrown by imports are catchable by Wasm's catch_all. A rejected
    // host call still fails the export, as a native host trap does; preserve the trap's panic
    // message too. Imports with a typed failure already recorded keep that exact error.
    const hostImports = imports[IMPORT_MODULE]!;
    for (const [name, value] of Object.entries(hostImports)) {
      if (typeof value !== "function") continue;
      hostImports[name] = (...args: number[]) => {
        if (program.#failure !== null) throw program.#failure;
        try {
          return value(...args);
        } catch (e) {
          program.#failure ??= new TrapError(trapMessage(e, memory, THREAD_MAIN, program.#lines));
          throw e;
        }
      };
    }
    // As the native host words it: a module that can't be instantiated isn't a valid program.
    let instance: WebAssembly.Instance;
    try {
      instance = await WebAssembly.instantiate(module, imports);
    } catch (e) {
      const failure = program.#failure;
      program.#failure = null;
      throw failure ?? new ProgramError(errorMessage(e));
    }
    const startFailure = program.#failure;
    program.#failure = null;
    if (startFailure !== null) throw startFailure;
    if (instance.exports[EXPORT_MEMORY] !== memory) {
      throw new ProgramError(`its \`${EXPORT_MEMORY}\` export isn't its imported shared memory`);
    }
    program.#instance = instance;
    program.#module = module;
    program.#memory = memory;
    program.#reserved = (compiled.memory.maximum ?? compiled.memory.initial) * 65536;
    program.#frame = instance.exports[EXPORT_FRAME] as FrameFn;
    try {
      // The helpers: each its own instance of the module on its own thread, with the memory.
      if (EXPORT_WORKER in instance.exports) {
        const controls = PAR_HELPERS + 4;
        if (memory.buffer.byteLength < controls) {
          throw new ProgramError(`it exports \`${EXPORT_WORKER}\` but its memory is ${memory.buffer.byteLength} bytes; the helpers need at least ${controls}`);
        }
        const spawn = options.spawnWorker;
        const helpers = spawn ? Math.min(Math.max(options.workers ?? 1, 1), MAX_WORKERS + 1) - 1 : 0;
        for (let i = 0; i < helpers; i++) {
          spawn!(module, memory, THREAD_HELPER0 + i, (line) => program.#printed(line));
        }
        // How many helpers there are: with none, std runs a task when asked if it's done.
        new DataView(memory.buffer).setUint32(PAR_HELPERS, helpers, true);
      }
      if (options.tickHashes && memory.buffer.byteLength >= TICK_WANT_HASH + 4) {
        new DataView(memory.buffer).setUint32(TICK_WANT_HASH, 1, true);
      }
      // A program with state makes it once, before anything else runs (language.md §15).
      const init = instance.exports[EXPORT_INIT];
      if (typeof init === "function") program.#call(() => (init as () => void)());
      program.#ready = true;
      program.#startThreads();
    } catch (e) {
      // Helpers may already be waiting in `__worker`; no thread started by a rejected program
      // may outlive it. Voice and ticker callbacks haven't escaped yet.
      program.shutdown();
      throw e;
    }
    return program;
  }

  /** Decodes and checks one owned batch, keeping its commands until the export succeeds. The
   * state hash covers every submitted byte, valid or not. */
  #submit(batch: Bytes): void {
    this.hash?.update(batch);
    for (const cmd of decode(batch)) {
      if (this.#failed !== null) {
        throw new CommandError(cmd.op, `the host failed to carry out an earlier command (${this.#failed})`);
      }
      // A command the sequencer or the checker refuses fails the call, which puts the
      // sequencer back as it was before the call (`#call`).
      this.#sequencer.step(cmd);
      if (this.#checking) this.checker.check(cmd);
      else checkHostRules(cmd);
      if (cmd.op === "HostRequest" && this.#net && cmd.service.startsWith("net/")) {
        this.#netCommand(cmd.service, cmd.body, false);
      } else if ("request" in cmd) {
        this.#requests.reserve(cmd.op, cmd.request);
      }
      this.#pending.push(cmd);
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
      case "Timed": {
        const time = this.executor.timeLabel?.bind(this.executor);
        this.#requests.start(cmd.op, cmd.request, time ? () => time(cmd.label) : none("GPU whose work it times"));
        return;
      }
      case "StorageWrite": {
        // Give the request its own bytes, independent of the staged batch.
        const data = cmd.data.slice();
        const write = io ? () => io.storageWrite(cmd.path, data).then(() => new Uint8Array(0)) : none("storage");
        this.#requests.start(cmd.op, cmd.request, write);
        return;
      }
      case "Fetch":
        this.#requests.start(cmd.op, cmd.request, io ? () => io.fetch(cmd.url) : none("build to fetch from"));
        return;
      case "Post": {
        // Give the request its own bytes, independent of the staged batch.
        const body = cmd.body.slice();
        this.#requests.start(cmd.op, cmd.request, io ? () => io.post(cmd.url, body) : none("server to post to"));
        return;
      }
      case "HostRequest": {
        if (this.#net && cmd.service.startsWith("net/")) {
          this.#netCommand(cmd.service, cmd.body);
          return;
        }
        const body = cmd.body.slice();
        const host = io?.hostRequest;
        this.#requests.start(cmd.op, cmd.request, host ? () => host.call(io, cmd.service, body) : none("host capabilities"));
        return;
      }
      case "Prepare": {
        const builds = this.#builds;
        const prepare = builds ? () => builds.prepare(cmd.path).then(() => new Uint8Array(0)) : none(OTHER_BUILD);
        this.#requests.start(cmd.op, cmd.request, prepare);
        return;
      }
      case "Unprepare":
        this.#builds?.unprepare(cmd.path);
        return;
      case "HandOver": {
        const builds = this.#builds;
        const go = builds ? () => builds.handOver(cmd.path).then(() => new Uint8Array(0)) : none(OTHER_BUILD);
        this.#requests.start(cmd.op, cmd.request, go);
        return;
      }
      default:
        return;
    }
  }

  /** A network command (`std::net`): checks the export's planned connections, or carries
   * it out at commit. It is never answered, so it holds nothing for the program to take. */
  #netCommand(service: string, body: Bytes, execute = true): void {
    const net = execute ? this.#net! : (this.#netPlan ??= this.#net!.plan());
    switch (service) {
      case "net/open": {
        let room: string;
        try {
          // A copy: the body is a view into the program's shared memory, which a decoder refuses.
          room = UTF8.decode(body.slice(4));
        } catch {
          throw new TrapError("`net/open`'s room isn't UTF-8");
        }
        try {
          const conn = connOf(service, body);
          net.open(conn, room, this.#pair);
        } catch (e) {
          throw new TrapError(errorMessage(e));
        }
        return;
      }
      case "net/send":
        net.send(connOf(service, body), body.subarray(4));
        return;
      case "net/close":
        net.close(connOf(service, body));
        return;
      default:
        throw new TrapError(`\`${service}\` isn't a network command this host has`);
    }
  }

  /** `wrela.request_status(request)`: -1 while it's pending, -2 if it failed, -3 if it was a
   * storage read of a path that holds nothing, else its answer's length in bytes. */
  #requestStatus(request: number): number {
    this.#starting();
    const a = this.#requests.answered.get(request);
    if (a === undefined) return REQUEST_PENDING;
    if ("error" in a) return a.missing ? REQUEST_MISSING : REQUEST_FAILED;
    return a.bytes.length;
  }

  /** `wrela.request_take(request, ptr)`: copies an answered request's bytes to `ptr`, and
   * forgets the request (a failed one too). */
  #requestTake(request: number, ptr: number): void {
    this.#starting();
    const a = this.#requests.answered.get(request);
    if (a === undefined) throw new TrapError(`request_take(${request}): the request isn't answered`);
    const bytes = "bytes" in a ? a.bytes : new Uint8Array(0);
    const memory = new Uint8Array(this.#memory!.buffer);
    if (ptr + bytes.length > memory.length) {
      throw new TrapError(`request_take(${request}, ${ptr}) reaches past the end of the program's memory`);
    }
    memory.set(bytes, ptr);
    this.#requests.answered.delete(request);
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
      // The export may overwrite its command buffer after this import returns, or trap after
      // submitting several batches. Keep each batch's bytes until the export can commit it.
      this.#submit(memory.slice(p, p + n));
    } catch (e) {
      this.#failure = e;
      throw e;
    }
  }

  /** Queues input events (EVENT_SIZE bytes each): the program reads them from its next call
   * on (runtime/abi `input`). */
  queueInput(events: Uint8Array): void {
    for (let at = 0; at + EVENT_SIZE <= events.length; at += EVENT_SIZE) {
      this.#inbox.push(events.slice(at, at + EVENT_SIZE));
    }
  }

  /** Queues a script's event: a message with its bytes. */
  queueScripted(s: Scripted): void {
    if (s.payload === undefined) this.#inbox.push(s.event);
    else this.#inbox.pushMessage(new DataView(s.event.buffer, s.event.byteOffset, EVENT_SIZE).getUint32(8, true), s.payload);
  }

  /** `wrela.input(ptr, cap) -> count`: copies up to `cap` queued events to `ptr`. */
  #inputImport(ptr: number, cap: number): number {
    this.#starting();
    const n = Math.min(cap, this.#inbox.length);
    const memory = new Uint8Array(this.#memory!.buffer);
    if (ptr + n * EVENT_SIZE > memory.length) {
      throw new TrapError(`input(${ptr}, ${cap}) reaches past the end of the program's memory`);
    }
    const events = this.#inbox.take(n);
    events.forEach((e, i) => memory.set(e, ptr + i * EVENT_SIZE));
    return events.length;
  }

  /** `wrela.payload(serial_lo, serial_hi, ptr, cap) -> length`: copies up to `cap` of a message's
   * bytes to `ptr`, if this call was given it; -1 if it wasn't. */
  #payloadImport(lo: number, hi: number, ptr: number, cap: number): number {
    this.#starting();
    const bytes = this.#inbox.payload(lo + hi * 2 ** 32);
    if (bytes === undefined) return -1;
    const n = Math.min(bytes.length, cap);
    const memory = new Uint8Array(this.#memory!.buffer);
    if (ptr + n > memory.length) throw new TrapError(`payload(.., ${ptr}, ${cap}) reaches past the end of the program's memory`);
    memory.set(bytes.subarray(0, n), ptr);
    return bytes.length;
  }

  /** `wrela.keep(ptr, len)`: keeps those bytes for the build that replaces this one. */
  #keepImport(ptr: number, len: number): void {
    this.#starting();
    const memory = new Uint8Array(this.#memory!.buffer);
    if (ptr + len > memory.length) throw new TrapError(`keep(${ptr}, ${len}) reaches past the end of the program's memory`);
    // A program may keep its state every frame: the same length reuses the copy.
    if (this.#keeping.length === len) this.#keeping.set(memory.subarray(ptr, ptr + len));
    else this.#keeping = memory.slice(ptr, ptr + len);
  }

  /** `wrela.kept(ptr, cap) -> length`: copies up to `cap` bytes of what the replaced build
   * kept to `ptr`; how many it kept. */
  #keptImport(ptr: number, cap: number): number {
    this.#starting();
    const n = Math.min(cap, this.#kept.length);
    const memory = new Uint8Array(this.#memory!.buffer);
    if (ptr + n > memory.length) throw new TrapError(`kept(${ptr}, ${cap}) reaches past the end of the program's memory`);
    memory.set(this.#kept.subarray(0, n), ptr);
    return this.#kept.length;
  }

  /** Hot reload: what this build keeps for the one that replaces it, and its input not yet
   * read. */
  get keeping(): Uint8Array {
    return this.#keeping;
  }

  get inbox(): Inbox {
    return this.#inbox;
  }

  /** Hot reload of a literal: in a lifted build, literal `i` takes `value` from the next call
   * on (`__lift_set`). A build that isn't lifted has none to set. */
  setLiteral(i: number, value: number): void {
    const f = this.#instance?.exports[EXPORT_LIFT_SET];
    if (typeof f === "function") this.#call(() => (f as (i: number, v: number) => void)(i, value));
  }

  /** Stops the program's helpers (they see the flag when they wake): the program runs no
   * more (a hot reload replaces it). */
  shutdown(): void {
    if (!this.#memory) return;
    // A valid ABI can import an empty memory. There cannot be a helper waiting in memory that
    // does not contain the helper controls, and cleanup must not mask the error that rejected
    // such a program's initialization.
    if (this.#memory.buffer.byteLength <= PAR_WAKE) return;
    const words = new Int32Array(this.#memory.buffer);
    Atomics.store(words, PAR_SHUTDOWN / 4, 1);
    Atomics.add(words, PAR_WAKE / 4, 1);
    Atomics.notify(words, PAR_WAKE / 4);
  }

  /** `wrela.audio(task, context)`: starts the program's voice, once. */
  #audioImport(task: number, context: number): void {
    this.#starting();
    if (this.#voice !== null) this.#failTrap("a program starts one voice, and this one started a second");
    if (!(EXPORT_AUDIO in this.#instance!.exports)) this.#failTrap(`it starts a voice but doesn't export \`${EXPORT_AUDIO}\``);
    const need = AUDIO_OUT + AUDIO_CHANNELS * AUDIO_QUANTUM * 4;
    const got = this.#memory!.buffer.byteLength;
    if (got < need) {
      this.#failTrap(`it starts a voice but its memory is ${got} bytes; the audio thread needs at least ${need}`);
    }
    this.#voice = { task, context };
    if (this.#ready) this.#startVoiceNow();
  }

  /** Whether the program has started its voice. */
  get hasVoice(): boolean {
    return this.#voice !== null;
  }

  /** `wrela.tick(task, context, hz)`: starts the program's ticker, once: a second is its panic. */
  #tickImport(task: number, context: number, hz: number): void {
    this.#starting();
    if (this.#ticker !== null) this.#failTrap("panic: a program starts one ticker, and this one started a second");
    if (!(EXPORT_TICK in this.#instance!.exports)) this.#failTrap(`it starts a ticker but doesn't export \`${EXPORT_TICK}\``);
    if (hz === 0) this.#failTrap("it starts a ticker at 0 Hz; a ticker ticks at least once a second");
    const need = TICK_RECORDS + 4 + MAX_TICK_RECORDS * EVENT_SIZE;
    const got = this.#memory!.buffer.byteLength;
    if (got < need) {
      this.#failTrap(`it starts a ticker but its memory is ${got} bytes; the ticker needs at least ${need}`);
    }
    this.#ticker = { task, context, hz };
    if (this.#ready) this.#startTickerNow();
  }

  /** Initialization committed: starts the other threads it requested, now that it can't be
   * rejected with their side effects already outside the program. */
  #startThreads(): void {
    if (this.#voice !== null) this.#startVoiceNow();
    if (this.#ticker !== null) this.#startTickerNow();
  }

  #startVoiceNow(): void {
    const voice = this.#voice!;
    this.#startVoice?.(
      { module: this.#module!, memory: this.#memory!, ...voice },
      (line) => this.#printed(line),
    );
  }

  #startTickerNow(): void {
    const ticker = this.#ticker!;
    this.#startTicker?.(
      { module: this.#module!, memory: this.#memory!, ...ticker },
      (line) => this.#printed(line),
    );
  }

  /** Records a typed host failure before throwing it into Wasm, so `catch_all` can't make the
   * call appear to have succeeded (the native host's `fail`). */
  #failTrap(why: string): never {
    const failure = new TrapError(why);
    this.#failure = failure;
    throw failure;
  }

  /** A line printed on this program's thread, or relayed from one of its other threads. */
  #printed(line: string): void {
    this.#onPrint?.(line);
  }

  /** The ticker the program started, if it has: its task, context and rate. */
  get ticker(): Pick<TickerOptions, "task" | "context" | "hz"> | null {
    return this.#ticker;
  }

  /** Refuses a call from the module's start function, which runs while it's instantiated (the
   * compiler's copies the constants into the memory, wrela_abi memory's `DATA_READY`). */
  #starting(): void {
    if (this.#instance === null) {
      const failure = new ProgramError(CALLED_WHILE_STARTING);
      this.#failure = failure;
      throw failure;
    }
  }

  /** Calls `frame(time, width, height)`, then ends the frame. */
  frame(time: number, width: number, height: number): void {
    this.checker.setScreenSize(width, height);
    this.#call(() => this.#frame!(time, width, height));
  }

  /** The phases its calls timed since the last take (`std::time::phase`), ms by name; none
   * unless the options asked for them. */
  takePhases(): Record<string, number> {
    return this.#phases?.take() ?? {};
  }

  /** Its memory, in bytes: the maximum it reserves, and what it has grown to. */
  get memoryBytes(): { reserved: number; used: number } {
    return { reserved: this.#reserved, used: this.#memory?.buffer.byteLength ?? 0 };
  }

  /** How many chunks of parallel jobs the workers (not the program's own thread) have run. */
  workerChunks(): number {
    if (!this.#memory || this.#memory.buffer.byteLength < PAR_HELPED + 4) return 0;
    return Atomics.load(new Uint32Array(this.#memory.buffer), PAR_HELPED / 4);
  }

  /** Calls another export (for tests). */
  call(name: string, ...args: number[]): unknown {
    const f = this.#instance?.exports[name];
    if (typeof f !== "function") throw new ProgramError(`it has no exported function \`${name}\``);
    return this.#call(() => (f as (...a: number[]) => unknown)(...args));
  }

  #call<T>(f: () => T): T {
    this.#requests.settle();
    this.#failure = null;
    const before = this.#sequencer.copy();
    this.checker.beginCall();
    let result: T;
    try {
      try {
        result = f();
      } catch (e) {
        if (this.#failure !== null) throw this.#failure;
        throw new TrapError(trapMessage(e, this.#memory, THREAD_MAIN, this.#lines));
      }
      // A module can catch an import's exception; that rejected import still fails its call.
      if (this.#failure !== null) throw this.#failure;
      this.#sequencer.endFrame();
    } catch (e) {
      this.#pending.length = 0;
      this.#sequencer = before;
      this.checker.endCall(false);
      throw e;
    } finally {
      // A phase ends with its call, as on the native host.
      this.#phases?.end();
      this.#inbox.endCall();
      this.#requests.endCall();
      this.#netPlan = null;
      this.#failure = null;
    }
    this.checker.endCall(true);
    const pending = this.#pending;
    this.#pending = [];
    try {
      for (const cmd of pending) {
        this.executor.execute(cmd);
        this.#request(cmd);
      }
      this.executor.flush();
    } catch (e) {
      // An executor or host capability can fail during commit, after external effects have
      // begun. Its partial state cannot be rolled back, so no later commands may use it.
      this.#failed = errorMessage(e);
      throw e;
    }
    return result;
  }
}

/** Why thread `thread` trapped with `e`, as the native host reports it: the program's panic
 * message, if it left one in `memory`, then `describeTrap`. */
export function trapMessage(e: unknown, memory: WebAssembly.Memory | null, thread: number, lines: Lines | null): string {
  const msg = memory === null ? null : takePanicMessage(memory, thread);
  const what = describeTrap(e, lines);
  return msg === null ? what : `panic: ${msg}: ${what}`;
}

/** What trapped, and where in the source if the program says (`lines`) and the engine's stack
 * trace gives offsets: the location of the innermost frame that has one. Without the table (an
 * optimized build's is beside it), the frames' offsets, which the page looks up (`locate`). */
function describeTrap(e: unknown, lines: Lines | null): string {
  // A host import's TrapError crosses the Wasm boundary unchanged. Its prefix is this class's
  // presentation, not part of the trap reason that the outer TrapError presents again.
  const prefix = "the program trapped: ";
  const what = e instanceof TrapError && e.message.startsWith(prefix) ? e.message.slice(prefix.length) : errorMessage(e);
  const stack = e instanceof Error ? (e.stack ?? "") : "";
  const offsets = wasmOffsets(stack);
  if (lines === null) return offsets.length > 0 ? `${what} at ${unlocated(offsets)}` : what;
  const at = offsets.map((offset) => lines.at(offset)).find((loc) => loc !== null);
  return at ? `${what} at ${at}` : what;
}

/** The message a panic on thread `thread` left before it trapped (its block's `PANIC`), cleared
 * so a later trap that isn't a panic doesn't repeat it. */
function takePanicMessage(memory: WebAssembly.Memory, thread: number): string | null {
  const at = threadBlock(thread) + PANIC;
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
