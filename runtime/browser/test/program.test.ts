// The program ABI without a GPU, mirroring runtime/native/src/program/tests.rs.

import { describe, expect, spyOn, test } from "bun:test";
import { CommandError } from "../src/check.ts";
import { parseManifest } from "../src/manifest.ts";
import { checkAbi, Program, ProgramError, TrapError } from "../src/program.ts";
import { StateHash } from "../src/hash.ts";
import { type Command, StreamError } from "../src/stream.ts";
import { wasmOffsets } from "../src/lines.ts";
import { Encoder } from "./encoder.ts";
import { checker, Recorder, readFixture, readFixtureText, shapes } from "./fixtures.ts";
import { batchProgram, buildModule, FRAME_TYPE, type Import, op, SUBMIT } from "./wasm-builder.ts";

const U16 = new Uint8Array(16);

async function load(wasm: Uint8Array<ArrayBuffer>): Promise<Program> {
  return Program.load(wasm, checker(), new Recorder(), { hash: true });
}

/** Runs every frame of a batch program; returns the program and the first error. */
async function run(frames: Uint8Array[][]): Promise<{ program: Program; error: unknown }> {
  const program = await load(batchProgram(frames));
  try {
    frames.forEach((_, i) => program.frame(i / 60, 64, 64));
  } catch (error) {
    return { program, error };
  }
  return { program, error: null };
}

async function runError(frames: Uint8Array[][]): Promise<Error> {
  const { error } = await run(frames);
  if (!(error instanceof Error)) throw new Error("expected the run to fail");
  return error;
}

async function commandError(batch: Uint8Array): Promise<[string, string]> {
  const e = await runError([[batch]]);
  expect(e).toBeInstanceOf(CommandError);
  return [(e as CommandError).opcode, e.message];
}

const withBuffers = (f: (e: Encoder) => void) => {
  const e = new Encoder().createBuffer(1, 64).createBuffer(2, 64);
  f(e);
  return e.finish();
};

const frameOnly = { type: FRAME_TYPE, body: [], export: "frame" };

describe("the ABI check", () => {
  const rejects = async (wasm: Uint8Array<ArrayBuffer>, text: string) => {
    const promise = load(wasm);
    await expect(promise).rejects.toThrow(ProgramError);
    await expect(load(wasm)).rejects.toThrow(text);
  };

  test("imports only the host's functions", async () => {
    const log = { module: "env", name: "log", kind: "func" as const, type: { params: ["i32" as const], results: [] } };
    await rejects(
      buildModule({ imports: [log], memory: 1, funcs: [frameOnly] }),
            "invalid program: it imports `env.log`, which isn't one of the host's: `wrela.memory`, `wrela.submit`, `wrela.request_status`, `wrela.request_take`, `wrela.limit`, `wrela.audio`, `wrela.input`",
    );
    const wrongType = { ...SUBMIT, type: { params: ["i32" as const], results: [] } };
    await rejects(
      buildModule({ imports: [wrongType], memory: 1, funcs: [frameOnly] }),
      "its import `wrela.submit` must be a function (i32, i32) -> (), not a function (i32) -> ()",
    );
    await rejects(
      buildModule({ imports: [{ module: "wrela", name: "memory", kind: "memory" }], funcs: [frameOnly] }),
      "its import `wrela.memory` must be a shared memory, not a memory",
    );
    // Its own memory, rather than the shared one the host gives it.
    await rejects(
      buildModule({ memory: 1, ownMemory: true, funcs: [frameOnly] }),
      "it doesn't import its memory, shared, as `wrela.memory`",
    );
    await load(buildModule({ memory: 1, funcs: [frameOnly] })); // no imports is fine
  });

  test("checks each import's own type, not the last one of its name", async () => {
    const wrongType = { ...SUBMIT, type: { params: ["i64" as const], results: [] } };
    await rejects(
      buildModule({ imports: [wrongType, SUBMIT], memory: 1, funcs: [frameOnly] }),
      "its import `wrela.submit` must be a function (i32, i32) -> (), not a function (i64) -> ()",
    );
  });

  test("has a start function that doesn't call the host", async () => {
    // It runs while the module is instantiated, outside any call the host makes.
    const empty = new Encoder().finish();
    const start = { type: { params: [], results: [] }, body: [...op.i32(16), ...op.i32(empty.length), ...op.call(0)] };
    await rejects(
      buildModule({ imports: [SUBMIT], memory: 1, funcs: [frameOnly, start], data: [{ offset: 16, bytes: empty }], start: 2 }),
      "invalid program: its start function called the host; a wrela program calls the host only from an export",
    );
  });

  test("needs memory and frame exports", async () => {
    await rejects(buildModule({ memory: 1, exportMemory: false, funcs: [frameOnly] }), "it doesn't export its memory as `memory`");
    await rejects(buildModule({ memory: 1 }), "it must export `frame(time: f32, width: i32, height: i32)` with no results");
    await rejects(
      buildModule({ memory: 1, funcs: [{ type: { params: ["f64"], results: [] }, body: [], export: "frame" }] }),
      "it must export `frame(",
    );
    await expect(load(Uint8Array.from([1, 2, 3]))).rejects.toThrow(ProgramError);
  });

  test("a module that can't be instantiated is an invalid program, as natively", async () => {
    // A data segment past the end of its memory fails at instantiation.
    const wasm = buildModule({
      memory: 1,
      funcs: [frameOnly],
      data: [{ offset: 70_000, bytes: new Uint8Array([1]) }],
    });
    await expect(load(wasm)).rejects.toThrow(ProgramError);
  });

  test("accepts the first-light fixture", async () => {
    const wasm = readFixture("game.wasm");
    checkAbi(await WebAssembly.compile(wasm), wasm);
  });
});

test("hashes every submitted byte", async () => {
  const a = new Encoder().createBuffer(1, 16).finish();
  const b = new Encoder().beginScreenPass([0, 0, 0, 0]).present().finish();
  const c = new Encoder().finish();
  const { program, error } = await run([[a, b], [c]]);
  expect(error).toBeNull();
  const expected = new StateHash();
  for (const batch of [a, b, c]) expected.update(batch);
  expect(program.hash!.hex()).toBe(expected.hex());
  expect((program.executor as Recorder).log).toEqual(["CreateBuffer", "BeginScreenPass", "Present", "end", "end"]);
});

test("rejects another stream version", async () => {
  const batch = new Encoder().present().finish();
  batch[4] = 9;
  const e = await runError([[batch]]);
  expect(e).toBeInstanceOf(StreamError);
  expect((e as StreamError).kind).toBe("WrongVersion");
});

test("rejects malformed batches before running any command", async () => {
  const batch = new Encoder().createBuffer(1, 16).present().finish();
  batch[12 + 16] = 99;
  const { program, error } = await run([[batch]]);
  expect((error as StreamError).kind).toBe("UnknownOpcode");
  expect((program.executor as Recorder).log).toEqual([]);
});

test("rejects out-of-order commands; a pass may span batches", async () => {
  const draw = new Encoder().draw(0, 3, 1, [1, 2], U16).finish();
  expect(((await runError([[draw]])) as StreamError).kind).toBe("Sequence");
  const open = new Encoder().beginScreenPass([0, 0, 0, 0]).finish();
  expect(((await runError([[open]])) as StreamError).kind).toBe("UnclosedPass");
  const end = new Encoder().present().finish();
  expect((await run([[open, end]])).error).toBeNull();
});

test("rejects submits outside memory", async () => {
  const wasm = buildModule({
    imports: [SUBMIT],
    memory: 1,
    funcs: [{ type: FRAME_TYPE, body: [...op.i32(65530), ...op.i32(100), ...op.call(0)], export: "frame" }],
  });
  const program = await load(wasm);
  expect(() => program.frame(0, 1, 1)).toThrow(TrapError);
  expect(() => program.frame(0, 1, 1)).toThrow("submit(65530, 100) reaches past the end of the program's memory (65536 bytes)");
});

test("a rejected command leaves no trace", async () => {
  // The sequencer passes a `BeginScreenPass` the checker then rejects: no pass is open after
  // it. (The frame traps before it ends, so the next one sends the same batch: it's rejected
  // the same way, not as a second pass.)
  const nan = new Encoder().beginScreenPass([NaN, 0, 0, 1]).finish();
  const executor = new Recorder();
  const program = await Program.load(batchProgram([[nan]]), checker(), executor);
  expect(() => program.frame(0, 64, 64)).toThrow(CommandError);
  expect(() => program.frame(1 / 60, 64, 64)).toThrow("the clear colour's r isn't a finite number");
  expect(executor.log).toEqual([]);
});

test("nothing runs after the host fails a command", async () => {
  // What the executor holds after it fails isn't known: every later command fails.
  class FailsOnce extends Recorder {
    failed = false;
    override execute(cmd: Command): void {
      if (cmd.op === "Present" && !this.failed) {
        this.failed = true;
        throw new Error("lost");
      }
      super.execute(cmd);
    }
  }
  const pass = new Encoder().beginScreenPass([0, 0, 0, 0]).present().finish();
  const executor = new FailsOnce();
  const program = await Program.load(batchProgram([[pass], [pass]]), checker(), executor);
  expect(() => program.frame(0, 64, 64)).toThrow("lost");
  expect(() => program.frame(1 / 60, 64, 64)).toThrow("the host failed to carry out an earlier command (lost)");
  expect(executor.log).toEqual(["BeginScreenPass"]);
});

test("a failed submit fails the call even if the module catches it", async () => {
  const nan = new Encoder().beginScreenPass([NaN, 0, 0, 1]).finish();
  // try { submit(16, len) } catch_all {} (WASM exception handling)
  const body = [0x06, 0x40, ...op.i32(16), ...op.i32(nan.length), ...op.call(0), 0x19, 0x0b];
  const wasm = buildModule({
    imports: [SUBMIT],
    memory: 1,
    funcs: [{ type: FRAME_TYPE, body, export: "frame" }],
    data: [{ offset: 16, bytes: nan }],
  });
  const program = await load(wasm);
  expect(() => program.frame(0, 64, 64)).toThrow(CommandError);
});

test("reports program traps", async () => {
  const program = await load(buildModule({ memory: 1, funcs: [{ type: FRAME_TYPE, body: op.unreachable, export: "frame" }] }));
  expect(() => program.frame(0, 1, 1)).toThrow(TrapError);
});

describe("the checks, with the native host's messages", () => {
  test("buffers", async () => {
    expect(await commandError(new Encoder().createBuffer(1, 16).createBuffer(1, 16).finish())).toEqual([
      "CreateBuffer",
      "CreateBuffer failed: handle 1 already names a buffer",
    ]);
    expect((await commandError(new Encoder().createBuffer(1, 1 << 30).finish()))[1]).toBe(
      "CreateBuffer failed: buffer 1 is 1073741824 bytes; the limit is 134217728",
    );
    expect((await commandError(new Encoder().writeBuffer(3, 0, new Uint8Array(4)).finish()))[1]).toBe(
      "WriteBuffer failed: there's no buffer 3",
    );
    expect((await commandError(withBuffers((e) => e.writeBuffer(1, 60, new Uint8Array(8)))))[1]).toBe(
      "WriteBuffer failed: writing 8 bytes at offset 60 overruns buffer 1 (64 bytes)",
    );
    expect((await run([[withBuffers((e) => e.writeBuffer(1, 56, new Uint8Array(8)))]])).error).toBeNull();
  });

  test("dispatches", async () => {
    const dispatchError = async (f: (e: Encoder) => void) => (await commandError(withBuffers(f)))[1];
    expect(await dispatchError((e) => e.dispatch(7, [1, 1, 1], [1, 2], U16))).toBe(
      "Dispatch failed: there's no pipeline 7 (the manifest has 2)",
    );
    expect(await dispatchError((e) => e.dispatch(0, [1, 1, 1], [1, 2], U16))).toBe(
      "Dispatch failed: pipeline 0 (draw) is a render pipeline; Dispatch needs a compute pipeline",
    );
    expect(await dispatchError((e) => e.dispatch(1, [1, 1, 1], [1], U16))).toBe(
      "Dispatch failed: pipeline 1 (compute) binds 2 resources, but the command lists 1",
    );
    expect(await dispatchError((e) => e.dispatch(1, [1, 1, 1], [1, 2], new Uint8Array(8)))).toBe(
      "Dispatch failed: pipeline 1 (compute) takes 16 uniform bytes, but the command has 8",
    );
    expect(await dispatchError((e) => e.dispatch(1, [1, 1, 1], [1, 9], U16))).toBe("Dispatch failed: there's no buffer 9");
    expect(await dispatchError((e) => e.dispatch(1, [1, 1, 1], [1, 1], U16))).toBe(
      "Dispatch failed: buffer 1 is used both read-only and read-write in one dispatch",
    );
    expect(await dispatchError((e) => e.dispatch(1, [65536, 1, 1], [1, 2], U16))).toBe(
      "Dispatch failed: 65536x1x1 workgroups is over the limit of 65535 per dimension",
    );
    const separate = withBuffers((e) => e.dispatch(1, [1, 1, 1], [1, 2], U16).dispatch(1, [1, 1, 1], [2, 1], U16));
    expect((await run([[separate]])).error).toBeNull();
  });

  test("draws, across the whole pass", async () => {
    expect(await commandError(withBuffers((e) => e.beginScreenPass([0, 0, 0, 0]).draw(1, 3, 1, [1, 2], U16).present()))).toEqual([
      "Draw",
      "Draw failed: pipeline 1 (compute) is a compute pipeline; Draw needs a render pipeline",
    ]);
    const aliased = withBuffers((e) =>
      e.beginScreenPass([0, 0, 0, 0]).draw(0, 3, 1, [1, 2], U16).draw(0, 3, 1, [2, 1], U16).present(),
    );
    expect((await commandError(aliased))[1]).toBe(
      "Draw failed: buffer 2 is used both read-only and read-write in one pass",
    );
    const twoPasses = withBuffers((e) => {
      e.beginScreenPass([0, 0, 0, 0]).draw(0, 3, 1, [1, 2], U16).present();
      e.beginScreenPass([0, 0, 0, 0]).draw(0, 3, 1, [2, 1], U16).present();
    });
    expect((await run([[twoPasses]])).error).toBeNull();
  });
});

test("writable aliases and clear colours", async () => {
  // Pipeline 0 renders and pipeline 1 computes, each binding two read-write buffers.
  const m = shapes();
  for (const p of m.pipelines) {
    p.uniform = null;
    p.bindings = [
      { binding: 1, kind: "read_write", stage: "both", format: null },
      { binding: 2, kind: "read_write", stage: "both", format: null },
    ];
  }
  const why = async (manifest: typeof m, batch: Uint8Array) => {
    const program = await Program.load(batchProgram([[batch]]), checker(manifest), new Recorder());
    try {
      program.frame(0, 64, 64);
    } catch (e) {
      expect(e).toBeInstanceOf(CommandError);
      return (e as Error).message;
    }
    return null;
  };
  const none = new Uint8Array(0);
  expect(await why(m, withBuffers((e) => e.dispatch(1, [1, 1, 1], [1, 1], none)))).toBe(
    "Dispatch failed: buffer 1 is bound read-write twice in one dispatch",
  );
  expect(await why(m, withBuffers((e) => e.beginScreenPass([0, 0, 0, 0]).draw(0, 3, 1, [2, 2], none).present()))).toBe(
    "Draw failed: buffer 2 is bound read-write twice in one draw",
  );
  const apart = withBuffers((e) =>
    e.beginScreenPass([0, 0, 0, 0]).draw(0, 3, 1, [1, 2], none).draw(0, 3, 1, [2, 1], none).present(),
  );
  expect(await why(m, apart)).toBeNull();
  expect(await why(shapes(), new Encoder().beginScreenPass([NaN, 0.5, 0.25, 1]).present().finish())).toBe(
    "BeginScreenPass failed: the clear colour's r isn't a finite number",
  );
  expect(await why(shapes(), new Encoder().beginScreenPass([0, 0, 0, Infinity]).present().finish())).toBe(
    "BeginScreenPass failed: the clear colour's a isn't a finite number",
  );
});

test("calls other exports", async () => {
  const program = await load(readFixture("game.wasm"));
  for (const [x, want] of [[0, -1], [0.25, 0], [0.5, 1], [0.75, 0], [1.125, -0.5]] as const) {
    expect(program.call("tri", x)).toBe(want);
  }
  expect(() => program.call("nope")).toThrow("it has no exported function `nope`");
});

test("first-light's CPU side gives the native host's state hash", async () => {
  // runtime/native/tests/suite/gpu.rs derives this hash independently (with the Rust reference
  // encoder) and checks the native host computes it; suite/agreement.rs checks Chrome does.
  const manifest = parseManifest(readFixtureText("manifest.json"));
  const program = await Program.load(readFixture("game.wasm"), checker(manifest), new Recorder(), { hash: true });
  for (let i = 0; i < 60; i++) program.frame(i / 60, 640, 360);
  expect(program.hash!.hex()).toBe("b0550120310e39f1");
  const log = (program.executor as Recorder).log;
  expect(log.slice(0, 7)).toEqual(["CreateBuffer", "WriteBuffer", "Dispatch", "BeginScreenPass", "Draw", "Present", "end"]);
  expect(log.length).toBe(3 + 60 * 4);
});

test("runs without the state hash unless asked", async () => {
  const program = await Program.load(readFixture("game.wasm"), checker(), new Recorder());
  expect(program.hash).toBeNull();
});

test("reads the offsets of a stack trace's WASM frames", () => {
  // V8's form (Chrome), then SpiderMonkey's (Firefox).
  const v8 = "RuntimeError: divide by zero\n    at div (wasm://wasm/0a1b2c3d:wasm-function[7]:0x2f1)\n    at run (wasm://wasm/0a1b2c3d:wasm-function[9]:0x340)";
  expect(wasmOffsets(v8)).toEqual([0x2f1, 0x340]);
  expect(wasmOffsets("div@http://localhost/game.wasm:wasm-function[7]:0x2f1\n")).toEqual([0x2f1]);
  expect(wasmOffsets("<?>.wasm-function[7]@[wasm code]")).toEqual([]);
});

describe("requests", () => {
  const STATUS = { module: "wrela", name: "request_status", kind: "func" as const, type: { params: ["i32" as const], results: ["i32" as const] } };
  const TAKE = { module: "wrela", name: "request_take", kind: "func" as const, type: { params: ["i32" as const, "i32" as const], results: [] } };
  const I32 = { params: [], results: ["i32" as const] };
  const NONE_TYPE = { params: [], results: [] };

  /** A program whose `ask` submits `batch`, `status` is request 4's status, and `take` takes
   * its answer into memory and returns the answer's first word. */
  const program = (batch: Uint8Array, io?: Parameters<typeof Program.load>[3]) =>
    Program.load(
      buildModule({
        imports: [SUBMIT, STATUS, TAKE],
        memory: 1,
        funcs: [
          { type: FRAME_TYPE, body: [], export: "frame" },
          { type: NONE_TYPE, body: [...op.i32(16), ...op.i32(batch.length), ...op.call(0)], export: "ask" },
          { type: I32, body: [...op.i32(4), ...op.call(1)], export: "status" },
          { type: I32, body: [...op.i32(4), ...op.i32(1024), ...op.call(2), ...op.i32(1024), 0x28, 0x02, 0x00], export: "take" },
        ],
        data: [{ offset: 16, bytes: batch }],
      }),
      checker(),
      new Recorder(),
      io,
    );
  const later = () => new Promise((r) => setTimeout(r, 0));

  test("are answered from a later call, then taken once", async () => {
    const io = {
      fetch: async (url: string) => (url === "data/level.bin" ? Uint8Array.of(1, 2, 3, 4, 5) : Promise.reject(new Error("no such file"))),
      storageRead: async () => new Uint8Array(0),
      storageWrite: async () => {},
      post: async () => new Uint8Array(0),
    };
    const p = await program(new Encoder().fetch(4, "data/level.bin").finish(), { io });
    p.call("ask");
    expect(p.call("status")).toBe(-1);
    await later();
    expect(p.call("status")).toBe(5);
    expect(p.call("take")).toBe(0x04030201);
    expect(p.call("status")).toBe(-1);
  });

  test("fail without a place to go, and a request number is used once at a time", async () => {
    const p = await program(new Encoder().storageRead(4, "saves/slot1").finish());
    p.call("ask");
    expect(() => p.call("ask")).toThrow("StorageRead failed: request 4 is already in use");
    await later();
    expect(p.call("status")).toBe(-2);
    expect(p.call("take")).toBe(0);
    expect(() => p.call("take")).toThrow("request_take(4): the request isn't answered");
  });

  test("a readback without a GPU fails", async () => {
    const batch = new Encoder().createBuffer(1, 16).readBuffer(4, 1, 0, 8).destroyBuffer(1).finish();
    const p = await program(batch);
    p.call("ask");
    await later();
    expect(p.call("status")).toBe(-2);
  });

  test("storage paths stay inside the program's storage", async () => {
    const p = await program(new Encoder().storageWrite(4, "../escape", Uint8Array.of(1)).finish());
    expect(() => p.call("ask")).toThrow("StorageWrite failed: the storage path `../escape` has an empty, `.` or `..` part");
  });
});

describe("the voice", () => {
  const AUDIO = { module: "wrela", name: "audio", kind: "func" as const, type: { params: ["i32" as const, "i32" as const], results: [] } };
  // `init` starts the voice (task 3, context 100); `again` starts another.
  const start = [...op.i32(3), ...op.i32(100), ...op.call(0)];
  const wasm = buildModule({
    imports: [AUDIO],
    memory: 1,
    funcs: [
      frameOnly,
      { type: { params: [], results: [] }, body: start, export: "init" },
      { type: { params: [], results: [] }, body: start, export: "again" },
      { type: { params: ["i32", "i32"], results: [] }, body: [], export: "__audio" },
    ],
  });

  test("is handed to the audio thread once", async () => {
    const voices: { task: number; context: number }[] = [];
    const p = await Program.load(wasm, checker(), new Recorder(), {
      startVoice: (v) => voices.push({ task: v.task, context: v.context }),
    });
    expect(voices).toEqual([{ task: 3, context: 100 }]);
    expect(p.hasVoice).toBe(true);
    expect(() => p.call("again")).toThrow("a program starts one voice, and this one started a second");
  });
});

test("a printed line goes to the console as it's made, prefixed, and a phase is ignored", async () => {
  const PRINT: Import = { module: "wrela", name: "print", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  const PHASE: Import = { ...PRINT, name: "phase" };
  const line = new TextEncoder().encode("frame 1 é");
  // phase("x"); print(line); then a trap: the line is shown though the frame never ends.
  const body = [...op.i32(16), ...op.i32(1), ...op.call(1), ...op.i32(32), ...op.i32(line.length), ...op.call(0), ...op.unreachable];
  const printedLines: string[] = [];
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    const program = await Program.load(
      buildModule({
        imports: [PRINT, PHASE],
        memory: 1,
        funcs: [{ type: FRAME_TYPE, body, export: "frame" }],
        data: [{ offset: 16, bytes: Uint8Array.of(120) }, { offset: 32, bytes: line }],
      }),
      checker(),
      new Recorder(),
      { onPrint: (l) => printedLines.push(l) },
    );
    expect(() => program.frame(0, 64, 64)).toThrow();
    expect(log).toHaveBeenCalledWith("wrela: frame 1 é");
    expect(printedLines).toEqual(["frame 1 é"]);
  } finally {
    log.mockRestore();
  }
});

describe("posts", () => {
  const STATUS = { module: "wrela", name: "request_status", kind: "func" as const, type: { params: ["i32" as const], results: ["i32" as const] } };
  const TAKE = { module: "wrela", name: "request_take", kind: "func" as const, type: { params: ["i32" as const, "i32" as const], results: [] } };
  const I32 = { params: [], results: ["i32" as const] };
  const program = (batch: Uint8Array, io?: Parameters<typeof Program.load>[3]) =>
    Program.load(
      buildModule({
        imports: [SUBMIT, STATUS, TAKE],
        memory: 1,
        funcs: [
          { type: FRAME_TYPE, body: [], export: "frame" },
          { type: { params: [], results: [] }, body: [...op.i32(16), ...op.i32(batch.length), ...op.call(0)], export: "ask" },
          { type: I32, body: [...op.i32(4), ...op.call(1)], export: "status" },
          { type: I32, body: [...op.i32(4), ...op.i32(1024), ...op.call(2), ...op.i32(1024), 0x28, 0x02, 0x00], export: "take" },
        ],
        data: [{ offset: 16, bytes: batch }],
      }),
      checker(),
      new Recorder(),
      io,
    );
  const later = () => new Promise((r) => setTimeout(r, 0));

  test("go to the host's post, and fail without one", async () => {
    const batch = new Encoder().post(4, "studio/echo", Uint8Array.of(1, 2, 3, 4)).finish();
    const io = {
      fetch: async () => new Uint8Array(0),
      storageRead: async () => new Uint8Array(0),
      storageWrite: async () => {},
      post: async (url: string, body: Uint8Array) => {
        expect(url).toBe("studio/echo");
        return Uint8Array.from(body).reverse();
      },
    };
    const p = await program(batch, { io });
    p.call("ask");
    await later();
    expect(p.call("status")).toBe(4);
    expect(p.call("take")).toBe(0x01020304);
    const q = await program(batch);
    q.call("ask");
    await later();
    expect(q.call("status")).toBe(-2);
    const elsewhere = await program(new Encoder().post(4, "https://example.com/x", new Uint8Array(0)).finish());
    expect(() => elsewhere.call("ask")).toThrow("must be relative");
  });
});

describe("input", () => {
  const INPUT = { module: "wrela", name: "input", kind: "func" as const, type: { params: ["i32" as const, "i32" as const], results: ["i32" as const] } };
  // `take(cap)` reads up to `cap` events to 1024; `word(at)` reads a word; `pastTheEnd` reads one
  // event where it doesn't fit.
  const wasm = buildModule({
    imports: [INPUT],
    memory: 1,
    funcs: [
      { type: FRAME_TYPE, body: [], export: "frame" },
      { type: { params: ["i32"], results: ["i32"] }, body: [...op.i32(1024), 0x20, 0x00, ...op.call(0)], export: "take" },
      { type: { params: ["i32"], results: ["i32"] }, body: [0x20, 0x00, 0x28, 0x02, 0x00], export: "word" },
      { type: { params: [], results: ["i32"] }, body: [...op.i32(65530), ...op.i32(1), ...op.call(0)], export: "pastTheEnd" },
    ],
  });

  test("is read oldest first, and what isn't read waits", async () => {
    const { pointerEvent, textEvent, keyEvent, bits } = await import("../src/input.ts");
    const p = await Program.load(wasm, checker(), new Recorder());
    p.queueInput(new Uint8Array([...pointerEvent(2, 3.5, 4, 2, 1), ...textEvent(0xe9), ...keyEvent(true, 40, false, 0)]));
    expect(p.call("take", 2)).toBe(2);
    expect(p.call("word", 1024)).toBe(2);
    expect(p.call("word", 1028)).toBe(1);
    expect(p.call("word", 1032)).toBe(bits(3.5) | 0);
    expect(p.call("word", 1040)).toBe(2);
    expect(p.call("word", 1024 + 24 + 8)).toBe(0xe9);
    expect(p.call("take", 8)).toBe(1);
    expect(p.call("word", 1024)).toBe(5);
    expect(p.call("take", 8)).toBe(0);
    p.queueInput(textEvent(120));
    expect(() => p.call("pastTheEnd")).toThrow("past the end of the program's memory");
  });
});
