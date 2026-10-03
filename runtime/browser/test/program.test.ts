// The program ABI without a GPU, mirroring runtime/native/src/program/tests.rs.

import { describe, expect, test } from "bun:test";
import { CommandError } from "../src/check.ts";
import { parseManifest } from "../src/manifest.ts";
import { checkAbi, Program, ProgramError, TrapError } from "../src/program.ts";
import { StateHash } from "../src/hash.ts";
import { type Command, StreamError } from "../src/stream.ts";
import { wasmOffsets } from "../src/lines.ts";
import { Encoder } from "./encoder.ts";
import { checker, Recorder, readFixture, readFixtureText, shapes } from "./fixtures.ts";
import { batchProgram, buildModule, FRAME_TYPE, op, SUBMIT } from "./wasm-builder.ts";

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

  test("imports only wrela.submit", async () => {
    const log = { module: "env", name: "log", kind: "func" as const, type: { params: ["i32" as const], results: [] } };
    await rejects(
      buildModule({ imports: [log], memory: 1, funcs: [frameOnly] }),
      "invalid program: it imports `env.log`, but a wrela program may import only `wrela.submit`",
    );
    const wrongType = { ...SUBMIT, type: { params: ["i32" as const], results: [] } };
    await rejects(
      buildModule({ imports: [wrongType], memory: 1, funcs: [frameOnly] }),
      "its import `wrela.submit` must be a function (i32, i32) -> (), not a function (i32) -> ()",
    );
    await rejects(
      buildModule({ imports: [{ module: "wrela", name: "memory", kind: "memory" }], funcs: [frameOnly] }),
      "it imports `wrela.memory`",
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

  test("has no start function", async () => {
    // It would submit before the host could read its memory.
    const empty = new Encoder().finish();
    const start = { type: { params: [], results: [] }, body: [...op.i32(16), ...op.i32(empty.length), ...op.call(0)] };
    await rejects(
      buildModule({ imports: [SUBMIT], memory: 1, funcs: [frameOnly, start], data: [{ offset: 16, bytes: empty }], start: 2 }),
      "invalid program: it has a start function; a wrela program may run only when the host calls it",
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
      "CreateBuffer failed: buffer 1 already exists",
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
      "Dispatch failed: pipeline 1 (compute) binds 2 buffers, but the command lists 1",
    );
    expect(await dispatchError((e) => e.dispatch(1, [1, 1, 1], [1, 2], new Uint8Array(8)))).toBe(
      "Dispatch failed: pipeline 1 (compute) takes 16 uniform bytes, but the command has 8",
    );
    expect(await dispatchError((e) => e.dispatch(1, [1, 1, 1], [1, 9], U16))).toBe("Dispatch failed: there's no buffer 9");
    expect(await dispatchError((e) => e.dispatch(1, [1, 1, 1], [1, 1], U16))).toBe(
      "Dispatch failed: buffer 1 is bound both read-only and read-write in one dispatch",
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
      "Draw failed: buffer 2 is bound both read-only and read-write in one screen pass",
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
    p.buffers = [
      { binding: 1, access: "read_write" },
      { binding: 2, access: "read_write" },
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
  expect(program.hash!.hex()).toBe("affac621a26ab564");
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
