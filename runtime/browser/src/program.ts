// The CPU side: a program's WASM, its one import, and the batches it submits. Mirrors
// runtime/native/src/program.rs: the same ABI checks before instantiating, the same order of
// work for each batch (hash, decode, sequence, check, execute), the same errors.

import { EXPORT_FRAME, EXPORT_MEMORY, IMPORT_MODULE, IMPORT_SUBMIT } from "./abi.gen.ts";
import type { Checker } from "./check.ts";
import { StateHash } from "./hash.ts";
import { Lines, LINES_SECTION, wasmOffsets } from "./lines.ts";
import { type Bytes, type Command, decode, Sequencer } from "./stream.ts";
import { functionTypes } from "./wasm.ts";

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
}

/**
 * Checks a module against the program ABI before instantiating it: it may import only
 * `wrela.submit(i32, i32)`, and must export `memory` and `frame(f32, i32, i32)`. `bytes` is the
 * module's binary, for the function types JavaScript can't otherwise see.
 */
export function checkAbi(module: WebAssembly.Module, bytes: Uint8Array): void {
  let types: ReturnType<typeof functionTypes>;
  try {
    types = functionTypes(bytes);
  } catch (e) {
    throw new ProgramError(`can't read its function types: ${e instanceof Error ? e.message : String(e)}`);
  }
  for (const imp of WebAssembly.Module.imports(module)) {
    const name = `${imp.module}.${imp.name}`;
    if (imp.module !== IMPORT_MODULE || imp.name !== IMPORT_SUBMIT) {
      throw new ProgramError(
        `it imports \`${name}\`, but a wrela program may import only \`${IMPORT_MODULE}.${IMPORT_SUBMIT}\``,
      );
    }
    const ty = types.imports.get(name);
    if (imp.kind !== "function" || ty !== "(i32, i32) -> ()") {
      const what = imp.kind === "function" ? `a function ${ty}` : `a ${imp.kind}`;
      throw new ProgramError(`its import \`${name}\` must be a function (i32, i32) -> (), not ${what}`);
    }
  }
  const exports = new Map(WebAssembly.Module.exports(module).map((e) => [e.name, e.kind]));
  if (exports.get(EXPORT_MEMORY) !== "memory") {
    throw new ProgramError(`it doesn't export its memory as \`${EXPORT_MEMORY}\``);
  }
  if (exports.get(EXPORT_FRAME) !== "function" || types.exports.get(EXPORT_FRAME) !== "(f32, i32, i32) -> ()") {
    throw new ProgramError(`it must export \`${EXPORT_FRAME}(time: f32, width: i32, height: i32)\` with no results`);
  }
}

type FrameFn = (time: number, width: number, height: number) => void;

/** How a program is run. */
export interface ProgramOptions {
  /** Keep the state hash of every submitted byte (test mode reads it; it costs a pass over
   * each batch, so a game running normally doesn't keep it). */
  hash?: boolean;
}

/** A loaded program and everything that happens to its batches. */
export class Program {
  /** The state hash, when the options ask for it. */
  readonly hash: StateHash | null;
  readonly #sequencer = new Sequencer();
  #memory: WebAssembly.Memory | null = null;
  /** Where the code came from, for trap messages. */
  #lines: Lines | null = null;
  #frame: FrameFn | null = null;
  #instance: WebAssembly.Instance | null = null;
  /** The error behind the most recent trap raised by `submit`, so the caller gets it rather
   * than whatever the engine wraps it in. */
  #failure: unknown = null;

  private constructor(
    readonly checker: Checker,
    readonly executor: Executor,
    options: ProgramOptions,
  ) {
    this.hash = options.hash ? new StateHash() : null;
  }

  static async load(wasm: Bytes, checker: Checker, executor: Executor, options: ProgramOptions = {}): Promise<Program> {
    return Program.instantiate(await Program.compile(wasm), checker, executor, options);
  }

  /** Compiles a program's WASM and checks it against the program ABI. */
  static async compile(wasm: Bytes): Promise<WebAssembly.Module> {
    let module: WebAssembly.Module;
    try {
      module = await WebAssembly.compile(wasm);
    } catch (e) {
      throw new ProgramError(e instanceof Error ? e.message : String(e));
    }
    checkAbi(module, wasm);
    return module;
  }

  /** Instantiates a compiled program against the decoder. */
  static async instantiate(
    module: WebAssembly.Module,
    checker: Checker,
    executor: Executor,
    options: ProgramOptions = {},
  ): Promise<Program> {
    const program = new Program(checker, executor, options);
    try {
      program.#lines = Lines.of(module);
    } catch (e) {
      throw new ProgramError(`its \`${LINES_SECTION}\` section is malformed: ${e instanceof Error ? e.message : String(e)}`);
    }
    const imports = {
      [IMPORT_MODULE]: { [IMPORT_SUBMIT]: (ptr: number, len: number) => program.#submitImport(ptr, len) },
    };
    const instance = await WebAssembly.instantiate(module, imports);
    program.#instance = instance;
    program.#memory = instance.exports[EXPORT_MEMORY] as WebAssembly.Memory;
    program.#frame = instance.exports[EXPORT_FRAME] as FrameFn;
    return program;
  }

  /** Decodes and runs one batch. The state hash covers every submitted byte, valid or not. */
  #submit(batch: Bytes): void {
    this.hash?.update(batch);
    for (const cmd of decode(batch)) {
      this.#sequencer.step(cmd);
      this.checker.check(cmd);
      this.executor.execute(cmd);
    }
  }

  #submitImport(ptr: number, len: number): void {
    try {
      // The import is (i32, i32): reinterpret as unsigned, as the native host does.
      const [p, n] = [ptr >>> 0, len >>> 0];
      const memory = new Uint8Array(this.#memory!.buffer);
      if (p + n > memory.length) {
        throw new TrapError(
          `submit(${p}, ${n}) reaches past the end of the program's memory (${memory.length} bytes)`,
        );
      }
      this.#submit(memory.subarray(p, p + n));
    } catch (e) {
      this.#failure = e;
      throw e;
    }
  }

  /** Calls `frame(time, width, height)`, then ends the frame. */
  frame(time: number, width: number, height: number): void {
    this.#call(() => this.#frame!(time, width, height));
    this.#sequencer.endFrame();
    this.executor.flush();
  }

  /** Calls another export (for tests). */
  call(name: string, ...args: number[]): unknown {
    const f = this.#instance?.exports[name];
    if (typeof f !== "function") throw new ProgramError(`it has no exported function \`${name}\``);
    return this.#call(() => (f as (...a: number[]) => unknown)(...args));
  }

  /** What trapped, and where in the source if the program says and the engine's stack trace
   * gives offsets: the location of the innermost frame that has one. */
  #describe(e: unknown): string {
    const what = e instanceof Error ? e.message : String(e);
    const stack = e instanceof Error ? (e.stack ?? "") : "";
    const at = wasmOffsets(stack)
      .map((offset) => this.#lines?.at(offset) ?? null)
      .find((loc) => loc !== null);
    return at ? `${what} at ${at}` : what;
  }

  #call<T>(f: () => T): T {
    this.#failure = null;
    try {
      return f();
    } catch (e) {
      const failure = this.#failure;
      this.#failure = null;
      if (failure !== null) throw failure;
      throw new TrapError(this.#describe(e));
    }
  }
}
