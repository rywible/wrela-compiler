// The program ABI without a GPU, mirroring runtime/native/src/program/tests.rs.

import { describe, expect, spyOn, test } from "bun:test";
import { AUDIO_CHANNELS, AUDIO_OUT, AUDIO_QUANTUM, EVENT_SIZE, MAX_TICK_RECORDS, PANIC, PAR_SHUTDOWN, RUNNING, SCREEN, STREAM_VERSION, TASK_SLOTS, THREAD_BLOCK_SIZE, THREAD_BLOCKS, THREAD_HELPER0, TICK_RECORDS } from "../src/abi.gen.ts";
import { CommandError } from "../src/check.ts";
import { parseManifest } from "../src/manifest.ts";
import { checkAbi, otherInstance, Program, ProgramError, runWorker, TrapError, trapMessage } from "../src/program.ts";
import { StateHash } from "../src/hash.ts";
import { type Command, StreamError } from "../src/stream.ts";
import { Lines, locate, wasmOffsets } from "../src/lines.ts";
import { Net } from "../src/net.ts";
import { Encoder } from "./encoder.ts";
import { checker, Recorder, readFixture, readFixtureText, shapes, passOf } from "./fixtures.ts";
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
type FuncImport = Extract<Import, { kind: "func" }>;

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

  test("a start function can't call any host function", async () => {
    // It runs while the module is instantiated, outside any call the host makes.
    const imports: { host: FuncImport; args: number[] }[] = [
      { host: SUBMIT as FuncImport, args: [0, 0] },
      { host: { module: "wrela", name: "request_status", kind: "func", type: { params: ["i32"], results: ["i32"] } }, args: [0] },
      { host: { module: "wrela", name: "request_take", kind: "func", type: { params: ["i32", "i32"], results: [] } }, args: [0, 0] },
      { host: { module: "wrela", name: "limit", kind: "func", type: { params: ["i32"], results: ["i32"] } }, args: [0] },
      { host: { module: "wrela", name: "audio", kind: "func", type: { params: ["i32", "i32"], results: [] } }, args: [0, 0] },
      { host: { module: "wrela", name: "input", kind: "func", type: { params: ["i32", "i32"], results: ["i32"] } }, args: [0, 0] },
      { host: { module: "wrela", name: "tick", kind: "func", type: { params: ["i32", "i32", "i32"], results: [] } }, args: [0, 0, 0] },
      { host: { module: "wrela", name: "keep", kind: "func", type: { params: ["i32", "i32"], results: [] } }, args: [0, 0] },
      { host: { module: "wrela", name: "kept", kind: "func", type: { params: ["i32", "i32"], results: ["i32"] } }, args: [0, 0] },
      { host: { module: "wrela", name: "clock", kind: "func", type: { params: ["i32"], results: [] } }, args: [0] },
      { host: { module: "wrela", name: "print", kind: "func", type: { params: ["i32", "i32"], results: [] } }, args: [0, 0] },
      { host: { module: "wrela", name: "phase", kind: "func", type: { params: ["i32", "i32"], results: [] } }, args: [0, 0] },
    ];
    for (const { host, args } of imports) {
      const body = [...args.flatMap(op.i32), ...op.call(0), ...(host.type.results.length ? [0x1a] : [])];
      await rejects(
        buildModule({ imports: [host], memory: 1, funcs: [frameOnly, { type: { params: [], results: [] }, body }], start: 2 }),
        "invalid program: its start function called the host; a wrela program calls the host only from an export",
      );
    }
  });

  test("needs memory and frame exports", async () => {
    await rejects(buildModule({ memory: 1, exportMemory: false, funcs: [frameOnly] }), "it doesn't export its memory as `memory`");
    await rejects(buildModule({ memory: 1 }), "it must export `frame(time: f32, width: i32, height: i32)` with no results");
    await rejects(
      buildModule({ memory: 1, funcs: [{ type: { params: ["f64"], results: [] }, body: [], export: "frame" }] }),
      "it must export `frame(",
    );
    await rejects(
      buildModule({ memory: 1, exportOwnMemory: true, funcs: [frameOnly] }),
      "its `memory` export isn't its imported shared memory",
    );
    await expect(load(Uint8Array.from([1, 2, 3]))).rejects.toThrow(ProgramError);
  });

  test("checks optional thread-entry export types", async () => {
    const entries = [
      ["__worker", ["i32"]],
      ["__audio", ["i32", "i32", "i32"]],
      ["__tick", ["i32", "i32", "i32", "i32"]],
    ] as const;
    for (const [name, params] of entries) {
      const wasm = buildModule({
        memory: 1,
        funcs: [frameOnly, { type: { params: [], results: [] }, body: [], export: name }],
      });
      await rejects(wasm, `its \`${name}\` export must be a function (${params.join(", ")}) -> ()`);
    }
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
  batch[4] = STREAM_VERSION + 1;
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

test("initialization and other exports cannot return with an open pass", async () => {
  const batch = new Encoder().beginScreenPass([0, 0, 0, 1]).finish();
  const body = [...op.i32(16), ...op.i32(batch.length), ...op.call(0)];
  const module = (name: string) => buildModule({
    imports: [SUBMIT], memory: 1,
    funcs: [frameOnly, { type: { params: [], results: [] }, body, export: name }],
    data: [{ offset: 16, bytes: batch }],
  });
  await expect(load(module("init"))).rejects.toThrow("a call returned with a pass still open");
  const program = await load(module("open"));
  expect(() => program.call("open")).toThrow("a call returned with a pass still open");
});

test("initialization and other exports flush their completed command work", async () => {
  const created = new Encoder().createBuffer(1, 4).finish();
  const written = new Encoder().writeBuffer(1, 0, new Uint8Array(4)).finish();
  const submit = (offset: number, length: number) => [...op.i32(offset), ...op.i32(length), ...op.call(0)];
  const executor = new Recorder();
  const program = await Program.load(buildModule({
    imports: [SUBMIT], memory: 1,
    funcs: [frameOnly,
      { type: { params: [], results: [] }, body: submit(16, created.length), export: "init" },
      { type: { params: [], results: [] }, body: submit(16 + created.length, written.length), export: "write" },
    ],
    data: [{ offset: 16, bytes: created }, { offset: 16 + created.length, bytes: written }],
  }), checker(), executor);
  expect(executor.log).toEqual(["CreateBuffer", "end"]);
  program.call("write");
  expect(executor.log).toEqual(["CreateBuffer", "end", "WriteBuffer", "end"]);
});

test("a helper trap ignores absent controls and interior job pointers", async () => {
  const wasm = buildModule({ memory: 0, funcs: [frameOnly,
    { type: { params: ["i32"], results: [] }, body: op.unreachable, export: "__worker" },
  ] });
  const module = await WebAssembly.compile(wasm);
  const logged = spyOn(console, "error").mockImplementation(() => {});
  try {
    for (const initial of [0, 1]) {
      const memory = new WebAssembly.Memory({ initial, maximum: 20, shared: true });
      await expect(runWorker(module, memory, THREAD_HELPER0)).resolves.toBeUndefined();
    }
    for (const base of [THREAD_BLOCKS, TASK_SLOTS]) {
      const memory = new WebAssembly.Memory({ initial: 20, maximum: 20, shared: true });
      new DataView(memory.buffer).setUint32(THREAD_BLOCKS + THREAD_HELPER0 * THREAD_BLOCK_SIZE + RUNNING, base + 4, true);
      const bytes = new Uint8Array(memory.buffer, base, 32);
      const before = bytes.slice();
      await runWorker(module, memory, THREAD_HELPER0);
      expect(bytes).toEqual(before);
    }
  } finally {
    logged.mockRestore();
  }
});

test("a program without helper control memory reports no helper work", async () => {
  const program = await load(buildModule({ memory: 0, funcs: [frameOnly] }));
  expect(program.workerChunks()).toBe(0);
  program.frame(0, 1, 1);
  expect(program.workerChunks()).toBe(0);
});

test("requesting tick hashes accepts programs without ticker control memory", async () => {
  for (const memory of [0, 1]) {
    const program = await Program.load(buildModule({ memory, funcs: [frameOnly] }), checker(), new Recorder(), { tickHashes: true });
    expect(() => program.frame(0, 1, 1)).not.toThrow();
    expect(program.ticker).toBeNull();
  }
});

test("a helper export needs control memory before any helper is started", async () => {
  let spawned = 0;
  const wasm = buildModule({ memory: 0, funcs: [frameOnly,
    { type: { params: ["i32"], results: [] }, body: [], export: "__worker" },
  ] });
  await expect(Program.load(wasm, checker(), new Recorder(), {
    workers: 3, spawnWorker: () => { spawned++; },
  })).rejects.toThrow("it exports `__worker` but its memory is 0 bytes; the helpers need at least 276");
  expect(spawned).toBe(0);
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

test("a synchronous capability failure during commit prevents later commands", async () => {
  const bad = new Encoder().unprepare("floor").createBuffer(1, 4).finish();
  const next = new Encoder().writeBuffer(1, 0, new Uint8Array(4)).finish();
  const executor = new Recorder();
  const program = await Program.load(batchProgram([[bad], [next]]), checker(), executor, { builds: {
    prepare: async () => {}, handOver: async () => {}, unprepare: () => { throw new Error("capability lost"); },
  } });
  expect(() => program.frame(0, 1, 1)).toThrow("capability lost");
  expect(() => program.frame(1, 1, 1)).toThrow("the host failed to carry out an earlier command (capability lost)");
  expect(executor.log).toEqual(["Unprepare"]);
});

test("a failure while flushing committed work prevents later commands", async () => {
  class FailedFlush extends Recorder {
    override flush(): void { throw new Error("flush lost"); }
  }
  const executor = new FailedFlush();
  const program = await Program.load(batchProgram([
    [new Encoder().createBuffer(1, 4).finish()],
    [new Encoder().writeBuffer(1, 0, new Uint8Array(4)).finish()],
  ]), checker(), executor);
  expect(() => program.frame(0, 1, 1)).toThrow("flush lost");
  expect(() => program.frame(1, 1, 1)).toThrow("the host failed to carry out an earlier command (flush lost)");
  expect(executor.log).toEqual(["CreateBuffer"]);
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

test("a failed audio or ticker start fails the call even if the module catches it", async () => {
  const cases: { host: FuncImport; args: number[]; message: string }[] = [
    {
      host: { module: "wrela", name: "audio", kind: "func", type: { params: ["i32", "i32"], results: [] } },
      args: [0, 0],
      message: "the program trapped: it starts a voice but doesn't export `__audio`",
    },
    {
      host: { module: "wrela", name: "tick", kind: "func", type: { params: ["i32", "i32", "i32"], results: [] } },
      args: [0, 0, 60],
      message: "the program trapped: it starts a ticker but doesn't export `__tick`",
    },
  ];
  for (const { host, args, message } of cases) {
    const body = [0x06, 0x40, ...args.flatMap(op.i32), ...op.call(0), 0x19, 0x0b];
    const program = await Program.load(
      buildModule({ imports: [host], memory: 1, funcs: [{ type: FRAME_TYPE, body, export: "frame" }] }),
      checker(),
      new Recorder(),
    );
    let error: unknown = null;
    try {
      program.frame(0, 1, 1);
    } catch (e) {
      error = e;
    }
    expect(error).toBeInstanceOf(TrapError);
    expect((error as Error).message).toBe(message);
  }
});

test("invalid memory and request imports fail the call even if the module catches them", async () => {
  const cases: { name: string; args: number[]; result?: boolean; message: string }[] = [
    { name: "request_take", args: [9, 0], message: "request_take(9): the request isn't answered" },
    { name: "input", args: [65536, 1], result: true, message: "input(65536, 1) reaches past the end of the program's memory" },
    { name: "keep", args: [65536, 1], message: "keep(65536, 1) reaches past the end of the program's memory" },
    { name: "kept", args: [65536, 1], result: true, message: "kept(65536, 1) reaches past the end of the program's memory" },
    { name: "clock", args: [65536], message: "clock(65536) reaches past the end of the program's memory" },
    { name: "print", args: [65536, 1], message: "print(65536, 1) reaches past the end of the program's memory" },
    { name: "phase", args: [65536, 1], message: "phase(65536, 1) reaches past the end of the program's memory" },
  ];
  for (const { name, args, result, message } of cases) {
    const host: FuncImport = { module: "wrela", name, kind: "func", type: { params: args.map(() => "i32"), results: result ? ["i32"] : [] } };
    const body = [0x06, 0x40, ...args.flatMap(op.i32), ...op.call(0), ...(result ? [0x1a] : []), 0x19, 0x0b];
    const program = await Program.load(
      buildModule({ imports: [host], memory: 1, funcs: [{ type: FRAME_TYPE, body, export: "frame" }] }),
      checker(), new Recorder(), { kept: Uint8Array.of(1) },
    );
    program.queueInput(new Uint8Array(EVENT_SIZE));
    expect(() => program.frame(0, 1, 1)).toThrow(TrapError);
    expect(() => program.frame(0, 1, 1)).toThrow(message);
  }
});

test("a caught host trap prevents later host side effects in the same call", async () => {
  const KEEP: FuncImport = { module: "wrela", name: "keep", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  const PRINT: FuncImport = { ...KEEP, name: "print" };
  const program = await load(buildModule({
    imports: [KEEP, PRINT], memory: 1, funcs: [{ type: FRAME_TYPE, export: "frame", body: [
      0x06, 0x40, ...op.i32(65536), ...op.i32(1), ...op.call(0), 0x19, 0x0b,
      ...op.i32(16), ...op.i32(5), ...op.call(1),
    ] }], data: [{ offset: 16, bytes: new TextEncoder().encode("later") }],
  }));
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    expect(() => program.frame(0, 1, 1)).toThrow("keep(65536, 1)");
    expect(log).not.toHaveBeenCalled();
  } finally { log.mockRestore(); }
});

test("another thread cannot hide a refused host import or continue its host effects", async () => {
  const PRINT: FuncImport = { module: "wrela", name: "print", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  for (const printAfter of [false, true]) {
    const module = await WebAssembly.compile(buildModule({ imports: [SUBMIT, PRINT], memory: 1, funcs: [{
      type: { params: [], results: [] }, export: "run", body: [
        0x06, 0x40, ...op.i32(0), ...op.i32(0), ...op.call(0), 0x19, 0x0b,
        ...(printAfter ? [...op.i32(16), ...op.i32(5), ...op.call(1)] : []),
      ],
    }], data: [{ offset: 16, bytes: new TextEncoder().encode("later") }] }));
    const instance = await otherInstance(module, new WebAssembly.Memory({ initial: 1, maximum: 16384, shared: true }), "a helper");
    const log = spyOn(console, "log").mockImplementation(() => {});
    try {
      expect(() => (instance.exports.run as () => void)()).toThrow("a helper can't call the host");
      expect(log).not.toHaveBeenCalled();
    } finally { log.mockRestore(); }
  }
});

test("reports program traps", async () => {
  const program = await load(buildModule({ memory: 1, funcs: [{ type: FRAME_TYPE, body: op.unreachable, export: "frame" }] }));
  expect(() => program.frame(0, 1, 1)).toThrow(TrapError);
});

test("a trapping export commits none of its submitted commands", async () => {
  const batch = new Encoder().createBuffer(1, 16).finish();
  for (const name of ["init", "frame", "other"]) {
    const executor = new Recorder();
    const loading = Program.load(buildModule({
      imports: [SUBMIT], memory: 1,
      funcs: [...(name === "frame" ? [] : [frameOnly]), {
        type: name === "frame" ? FRAME_TYPE : { params: [], results: [] },
        body: [...op.i32(16), ...op.i32(batch.length), ...op.call(0), ...op.unreachable], export: name,
      }], data: [{ offset: 16, bytes: batch }],
    }), checker(), executor, { hash: true });
    if (name === "init") await expect(loading).rejects.toThrow(TrapError);
    else {
      const program = await loading;
      const call = () => name === "frame" ? program.frame(0, 1, 1) : program.call(name);
      expect(call).toThrow(TrapError);
      // The same handle can be checked again: no resource from the trapped call remains.
      expect(call).toThrow(TrapError);
      const hash = new StateHash();
      hash.update(batch);
      hash.update(batch);
      expect(program.hash!.hex()).toBe(hash.hex());
    }
    expect(executor.log).toEqual([]);
  }
});

test("a trapped call restores replaced resources and its open pass", async () => {
  const batches = [
    new Encoder().createBuffer(1, 64).createTexture(2, 1, 1, "rgba8unorm").createSampler(3, false, false, null).finish(),
    new Encoder().destroyBuffer(1).createTexture(1, 1, 1, "rgba8unorm")
      .destroyTexture(2).createBuffer(2, 4).destroySampler(3).createBuffer(3, 4)
      .createBuffer(4, 4).destroyBuffer(4).beginScreenPass([0, 0, 0, 1]).finish(),
    new Encoder().writeBuffer(1, 60, new Uint8Array(4)).destroyTexture(2).destroySampler(3)
      .createBuffer(4, 4).beginScreenPass([0, 0, 0, 1]).present().finish(),
  ];
  const data: { offset: number; bytes: Uint8Array }[] = [];
  let offset = 16;
  const funcs = batches.map((batch, i) => {
    const body = [...op.i32(offset), ...op.i32(batch.length), ...op.call(0), ...(i === 1 ? op.unreachable : [])];
    data.push({ offset, bytes: batch });
    offset += batch.length;
    return { type: { params: [], results: [] }, body, export: ["init", "bad", "recover"][i]! };
  });
  const executor = new Recorder();
  const program = await Program.load(buildModule({ imports: [SUBMIT], memory: 1, funcs: [frameOnly, ...funcs], data }), checker(), executor);
  const initialized = executor.log.slice();
  expect(() => program.call("bad")).toThrow(TrapError);
  expect(executor.log).toEqual(initialized);
  expect(() => program.call("recover")).not.toThrow();
  expect(executor.log.slice(initialized.length)).toEqual(["WriteBuffer", "DestroyTexture", "DestroySampler", "CreateBuffer", "BeginScreenPass", "Present", "end"]);
});

test("submitted bytes survive reuse of the program's command memory before return", async () => {
  const batch = new Encoder().createBuffer(1, 4).writeBuffer(1, 0, Uint8Array.of(1, 2, 3, 4)).finish();
  const uploaded: number[][] = [];
  class Uploads extends Recorder {
    override execute(cmd: Command): void {
      if (cmd.op === "WriteBuffer") uploaded.push([...cmd.data]);
      super.execute(cmd);
    }
  }
  const program = await Program.load(buildModule({ imports: [SUBMIT], memory: 1, funcs: [{
    type: FRAME_TYPE, export: "frame", body: [
      ...op.i32(16), ...op.i32(batch.length), ...op.call(0),
      ...op.i32(16 + batch.length - 4), ...op.i32(0x08070605), 0x36, 0x02, 0x00,
    ],
  }], data: [{ offset: 16, bytes: batch }] }), checker(), new Uploads());
  program.frame(0, 1, 1);
  expect(uploaded).toEqual([[1, 2, 3, 4]]);
});

test("a trapped call prints immediately while its command work is discarded", async () => {
  const PRINT: Import = { module: "wrela", name: "print", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  const batch = new Encoder().createBuffer(1, 4).finish();
  const line = new TextEncoder().encode("before trap");
  const executor = new Recorder();
  const program = await Program.load(buildModule({ imports: [SUBMIT, PRINT], memory: 1, funcs: [{
    type: FRAME_TYPE, export: "frame", body: [
      ...op.i32(16), ...op.i32(batch.length), ...op.call(0),
      ...op.i32(16 + batch.length), ...op.i32(line.length), ...op.call(1), ...op.unreachable,
    ],
  }], data: [{ offset: 16, bytes: batch }, { offset: 16 + batch.length, bytes: line }] }), checker(), executor);
  const log = spyOn(console, "log").mockImplementation(() => expect(executor.log).toEqual([]));
  try {
    expect(() => program.frame(0, 1, 1)).toThrow(TrapError);
    expect(log).toHaveBeenCalledWith("wrela: before trap");
    expect(executor.log).toEqual([]);
  } finally { log.mockRestore(); }
});

test("a trap without its table carries its offsets until the page locates it", async () => {
  // An optimized build's table is beside it (`game.lines`), not in its module. V8's stack (Bun's
  // engine writes no offsets; compiler/tests' optimize.rs traps in Chrome itself).
  const trap = new Error("divide by zero");
  trap.stack = "RuntimeError: divide by zero\n    at div (wasm://wasm/0a1b2c3d:wasm-function[7]:0x2f1)\n    at run (wasm://wasm/0a1b2c3d:wasm-function[9]:0x340)";
  const message = trapMessage(trap, null, 0, null);
  expect(message).toBe("divide by zero at wasm 0x2f1,0x340");
  // The innermost frame has no location; the next one has `main.wrela:3:5`.
  const location = new TextEncoder().encode("main.wrela:3:5");
  const table = Lines.decode(new Uint8Array([1, location.length, ...location, 2, 0, 0, 0x80, 0x06, 1]));
  expect(await locate(`the program trapped: ${message}`, async () => table)).toBe("the program trapped: divide by zero at main.wrela:3:5");
  // When the table doesn't arrive, the innermost offset alone.
  for (const load of [async () => null, async () => Promise.reject(new Error("offline"))]) {
    expect(await locate(message, load)).toBe("divide by zero at code offset 0x2f1");
  }
  // A message without offsets is left as it is, and nothing is fetched.
  let fetched = false;
  expect(await locate("can't fetch game.wasm", async () => ((fetched = true), table))).toBe("can't fetch game.wasm");
  expect(fetched).toBe(false);
});

test("a host TrapError has one prefix and still yields to the program's panic message", async () => {
  const KEEP: Import = { module: "wrela", name: "keep", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  const panic = new Uint8Array(8);
  new DataView(panic.buffer).setUint32(0, 4, true);
  panic.set(new TextEncoder().encode("oops"), 4);
  const program = await Program.load(
    buildModule({
      imports: [KEEP],
      memory: 20,
      funcs: [{ type: FRAME_TYPE, body: [...op.i32(0xffff_ffff), ...op.i32(1), ...op.call(0)], export: "frame" }],
      data: [{ offset: THREAD_BLOCKS + PANIC, bytes: panic }],
    }),
    checker(),
    new Recorder(),
  );
  let error: unknown = null;
  try {
    program.frame(0, 1, 1);
  } catch (e) {
    error = e;
  }
  expect(error).toBeInstanceOf(TrapError);
  expect((error as Error).message).toBe(
    "the program trapped: panic: oops: keep(4294967295, 1) reaches past the end of the program's memory",
  );
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
  // Stream v11 changes the batch headers; the native reference encoder checks the same bytes.
  expect(program.hash!.hex()).toBe("987ca2ba293937b5");
  const log = (program.executor as Recorder).log;
  expect(log.slice(0, 7)).toEqual(["CreateBuffer", "WriteBuffer", "Dispatch", "BeginScreenPass", "Draw", "Present", "end"]);
  expect(log.length).toBe(3 + 60 * 4);
});

test("runs without the state hash unless asked", async () => {
  const program = await Program.load(readFixture("game.wasm"), checker(), new Recorder());
  expect(program.hash).toBeNull();
});

test("a frame checks a screen depth target against its current size", async () => {
  const pass = passOf({ color: SCREEN, keepColor: false, join: false, clear: [0, 0, 0, 1] as [number, number, number, number], depth: 3, keepDepth: false, clearDepth: 1 });
  const batch = new Encoder().createTexture(3, 64, 63, "depth32float").beginPass(pass).present().finish();
  const p = await load(batchProgram([[batch]]));
  expect(() => p.frame(0, 64, 64)).toThrow("the pass's colour and depth targets differ in size");
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
          { type: NONE_TYPE, body: [...op.i32(4), ...op.i32(65534), ...op.call(2)], export: "takePastEnd" },
        ],
        data: [{ offset: 16, bytes: batch }],
      }),
      checker(),
      new Recorder(),
      io,
    );
  const later = () => new Promise((r) => setTimeout(r, 0));

  test("duplicate requests reject every staged effect and release their reservations", async () => {
    const bad = new Encoder().createBuffer(1, 4).fetch(4, "level").fetch(4, "level").finish();
    const good = new Encoder().createBuffer(1, 4).fetch(4, "level").finish();
    let fetched = 0;
    const executor = new Recorder();
    const p = await Program.load(buildModule({
      imports: [SUBMIT], memory: 1, funcs: [frameOnly,
        { type: NONE_TYPE, export: "bad", body: [0x06, 0x40, ...op.i32(16), ...op.i32(bad.length), ...op.call(0), 0x19, 0x0b] },
        { type: NONE_TYPE, export: "good", body: [...op.i32(16 + bad.length), ...op.i32(good.length), ...op.call(0)] },
      ], data: [{ offset: 16, bytes: bad }, { offset: 16 + bad.length, bytes: good }],
    }), checker(), executor, { io: {
      fetch: async () => { fetched++; return new Uint8Array(); },
      storageRead: async () => new Uint8Array(), storageWrite: async () => {}, post: async () => new Uint8Array(),
    } });
    expect(() => p.call("bad")).toThrow("request 4 is already in use");
    expect(executor.log).toEqual([]);
    expect(fetched).toBe(0);
    p.call("good");
    expect(executor.log).toEqual(["CreateBuffer", "Fetch", "end"]);
    expect(fetched).toBe(1);
  });

  test("a later invalid batch rejects the whole export before its requests start", async () => {
    const earlier = new Encoder().createBuffer(1, 4).fetch(4, "level").finish();
    const later = new Encoder().writeBuffer(1, 4, new Uint8Array(4)).finish();
    const executor = new Recorder();
    let fetched = 0;
    const p = await Program.load(buildModule({ imports: [SUBMIT], memory: 1, funcs: [{
      type: FRAME_TYPE, export: "frame", body: [
        ...op.i32(16), ...op.i32(earlier.length), ...op.call(0),
        ...op.i32(16 + earlier.length), ...op.i32(later.length), ...op.call(0),
      ],
    }], data: [{ offset: 16, bytes: earlier }, { offset: 16 + earlier.length, bytes: later }] }), checker(), executor, { io: {
      fetch: async () => { fetched++; return new Uint8Array(); },
      storageRead: async () => new Uint8Array(), storageWrite: async () => {}, post: async () => new Uint8Array(),
    } });
    expect(() => p.frame(0, 1, 1)).toThrow("overruns buffer 1");
    expect(executor.log).toEqual([]);
    expect(fetched).toBe(0);
  });

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

  test("an out-of-bounds take leaves the answer available for a valid take", async () => {
    const io = {
      fetch: async () => Uint8Array.of(1, 2, 3, 4, 5),
      storageRead: async () => new Uint8Array(0),
      storageWrite: async () => {},
      post: async () => new Uint8Array(0),
    };
    const p = await program(new Encoder().fetch(4, "data/level.bin").finish(), { io });
    p.call("ask");
    await later();
    expect(() => p.call("takePastEnd")).toThrow("request_take(4, 65534) reaches past the end of the program's memory");
    expect(p.call("status")).toBe(5);
    expect(p.call("take")).toBe(0x04030201);
    expect(p.call("status")).toBe(-1);
  });

  test("a storage read of nothing stored says so, and other failures don't", async () => {
    for (const [read, status] of [[async () => null, -3], [async () => Promise.reject(new Error("locked")), -2]] as const) {
      const io = { fetch: async () => new Uint8Array(0), storageRead: read, storageWrite: async () => {}, post: async () => new Uint8Array(0) };
      const p = await program(new Encoder().storageRead(4, "saves/slot1").finish(), { io });
      p.call("ask");
      await later();
      expect(p.call("status")).toBe(status);
      expect(p.call("take")).toBe(0);
      expect(p.call("status")).toBe(-1);
    }
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

  test("go to the host's other builds, and fail without them", async () => {
    const asked: string[] = [];
    const builds = {
      prepare: async (path: string) => {
        asked.push(`prepare ${path}`);
      },
      unprepare: (path: string) => asked.push(`unprepare ${path}`),
      handOver: (path: string) => Promise.reject(new Error(`no build at ${path}`)),
    };
    const p = await program(new Encoder().prepare(4, "../floor-2").unprepare("../floor-2").finish(), { builds });
    p.call("ask");
    await later();
    expect(asked).toEqual(["prepare ../floor-2", "unprepare ../floor-2"]);
    expect(p.call("status")).toBe(0);
    // A hand-over that can't be made fails its request, and the program goes on.
    const q = await program(new Encoder().handOver(4, "../floor-3").finish(), { builds });
    q.call("ask");
    await later();
    expect(q.call("status")).toBe(-2);
    const none = await program(new Encoder().prepare(4, "../floor-2").finish());
    none.call("ask");
    await later();
    expect(none.call("status")).toBe(-2);
  });

  test("a build's path stays on its site", async () => {
    const p = await program(new Encoder().handOver(4, "https://example.com/floor").finish());
    expect(() => p.call("ask")).toThrow("HandOver failed: the build path `https://example.com/floor`");
  });

  test("host capabilities answer opaque bytes later and fail without a handler", async () => {
    const io = { fetch: async () => new Uint8Array(), storageRead: async () => new Uint8Array(), storageWrite: async () => {}, post: async () => new Uint8Array(),
      hostRequest: async (service: string, body: Uint8Array) => { expect(service).toBe("test/echo"); expect(body).toEqual(Uint8Array.of(1, 2, 3, 4)); return body.slice().reverse(); } };
    const command = new Encoder().hostRequest(4, "test/echo", Uint8Array.of(1, 2, 3, 4)).finish();
    const p = await program(command, { io });
    p.call("ask");
    expect(p.call("status")).toBe(-1);
    await later();
    expect(p.call("status")).toBe(4);
    expect(p.call("take")).toBe(0x01020304);
    expect(p.call("status")).toBe(-1);
    const absent = await program(command);
    absent.call("ask");
    await later();
    expect(absent.call("status")).toBe(-2);
    const invalid = await program(new Encoder().hostRequest(4, "../escape", new Uint8Array()).finish(), { io });
    expect(() => invalid.call("ask")).toThrow("HostRequest failed");
  });

  test("network commands go to the connections at once, and are never answered", async () => {
    const word = (x: number) => Uint8Array.of(x & 255, (x >> 8) & 255, (x >> 16) & 255, x >>> 24);
    const done: string[] = [];
    const net = {
      plan: () => ({ open() {}, send() {}, close() {} }),
      open: (conn: number, room: string) => void done.push(`open ${conn} ${room}`),
      send: (conn: number, bytes: Uint8Array) => void done.push(`send ${conn} ${[...bytes].join(",")}`),
      close: (conn: number) => void done.push(`close ${conn}`),
      closeAll: () => void done.push("all"),
    };
    const batch = new Encoder()
      .hostRequest(4, "net/open", Uint8Array.of(...word(7), ...new TextEncoder().encode("board-1")))
      .hostRequest(5, "net/send", Uint8Array.of(...word(7), 1, 2, 3))
      .hostRequest(6, "net/close", word(7))
      .finish();
    const p = await program(batch, { net });
    p.call("ask");
    expect(done).toEqual(["open 7 board-1", "send 7 1,2,3", "close 7"]);
    await later();
    expect(p.call("status")).toBe(-1);
    const short = await program(new Encoder().hostRequest(4, "net/send", Uint8Array.of(1, 2)).finish(), { net });
    expect(() => short.call("ask")).toThrow("`net/send` needs a connection's number");
    const other = await program(new Encoder().hostRequest(4, "net/listen", word(1)).finish(), { net });
    expect(() => other.call("ask")).toThrow("`net/listen` isn't a network command this host has");
  });

  test("a duplicate network open fails during submit before any staged work commits", async () => {
    const PRINT: Import = { module: "wrela", name: "print", kind: "func", type: { params: ["i32", "i32"], results: [] } };
    const body = Uint8Array.of(7, 0, 0, 0, ...new TextEncoder().encode("room"));
    const batch = new Encoder().createBuffer(1, 4).hostRequest(4, "net/open", body).hostRequest(5, "net/open", body).finish();
    const wasm = buildModule({ imports: [SUBMIT, PRINT], memory: 1, funcs: [{ type: FRAME_TYPE, export: "frame", body: [
      0x06, 0x40, ...op.i32(16), ...op.i32(batch.length), ...op.call(0), 0x19, 0x0b,
      ...op.i32(0), ...op.i32(0), ...op.call(1),
    ] }], data: [{ offset: 16, bytes: batch }] });
    // A valid pair keeps the first open pending, so a second open of its number must fail.
    const paired = Uint8Array.from([...wasm, 0, 19, 10, ...new TextEncoder().encode("wrela.pair"), 1, 0, 0, 0, 0, 0, 0, 0]);
    const executor = new Recorder();
    const net = new Net(() => "https://site.example/", () => {});
    const fetched = spyOn(globalThis, "fetch").mockImplementation((() => new Promise<Response>(() => {})) as unknown as typeof fetch);
    const log = spyOn(console, "log").mockImplementation(() => {});
    try {
      const p = await Program.load(paired, checker(), executor, { net });
      expect(() => p.frame(0, 1, 1)).toThrow("connection 7 is open");
      expect(executor.log).toEqual([]);
      expect(fetched).not.toHaveBeenCalled();
      expect(log).not.toHaveBeenCalled();
    } finally {
      net.closeAll();
      fetched.mockRestore();
      log.mockRestore();
    }
  });

  test("network room text keeps a leading UTF-8 byte-order mark", async () => {
    const rooms: string[] = [];
    const room = "\uFEFFroom";
    const net = {
      plan: () => ({ open() {}, send() {}, close() {} }),
      open: (_conn: number, room: string) => { rooms.push(room); },
      send() {}, close() {}, closeAll() {},
    };
    const p = await program(new Encoder().hostRequest(4, "net/open", Uint8Array.of(7, 0, 0, 0, ...new TextEncoder().encode(room))).finish(), { net });
    p.call("ask");
    expect(rooms).toEqual([room]);
  });

  test("host capability methods retain their IO receiver", async () => {
    const io = {
      answer: Uint8Array.of(9, 8, 7, 6),
      fetch: async () => new Uint8Array(), storageRead: async () => new Uint8Array(),
      storageWrite: async () => {}, post: async () => new Uint8Array(),
      async hostRequest() { return this.answer.slice(); },
    };
    const p = await program(new Encoder().hostRequest(4, "test/echo", new Uint8Array()).finish(), { io });
    p.call("ask");
    await later();
    expect(p.call("status")).toBe(4);
    expect(p.call("take")).toBe(0x06070809);
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
    memory: 20,
    funcs: [
      frameOnly,
      { type: { params: [], results: [] }, body: start, export: "init" },
      { type: { params: [], results: [] }, body: start, export: "again" },
      { type: { params: ["i32", "i32", "i32"], results: [] }, body: [], export: "__audio" },
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

  test("needs memory for both output channels before it starts", async () => {
    const need = AUDIO_OUT + AUDIO_CHANNELS * AUDIO_QUANTUM * 4;
    const tooSmall = buildModule({
      imports: [AUDIO],
      memory: 19,
      funcs: [
        frameOnly,
        { type: { params: [], results: [] }, body: start, export: "init" },
        { type: { params: ["i32", "i32", "i32"], results: [] }, body: [], export: "__audio" },
      ],
    });
    let started = false;
    await expect(
      Program.load(tooSmall, checker(), new Recorder(), { startVoice: () => (started = true) }),
    ).rejects.toThrow(`it starts a voice but its memory is ${19 * 65536} bytes; the audio thread needs at least ${need}`);
    expect(started).toBe(false);
  });
});

test("a ticker needs its record region before it starts", async () => {
  const TICK: Import = {
    module: "wrela",
    name: "tick",
    kind: "func",
    type: { params: ["i32", "i32", "i32"], results: [] },
  };
  const need = TICK_RECORDS + 4 + MAX_TICK_RECORDS * EVENT_SIZE;
  const wasm = buildModule({
    imports: [TICK],
    memory: 18,
    funcs: [
      frameOnly,
      { type: { params: [], results: [] }, body: [...op.i32(3), ...op.i32(100), ...op.i32(60), ...op.call(0)], export: "init" },
      { type: { params: ["i32", "i32", "i32", "i32"], results: [] }, body: [], export: "__tick" },
    ],
  });
  let started = false;
  await expect(
    Program.load(wasm, checker(), new Recorder(), { startTicker: () => (started = true) }),
  ).rejects.toThrow(`it starts a ticker but its memory is ${18 * 65536} bytes; the ticker needs at least ${need}`);
  expect(started).toBe(false);
});

test("a ticker's rate is nonzero at the host boundary", async () => {
  const TICK: FuncImport = {
    module: "wrela",
    name: "tick",
    kind: "func",
    type: { params: ["i32", "i32", "i32"], results: [] },
  };
  const wasm = buildModule({
    imports: [TICK],
    memory: 20,
    funcs: [
      frameOnly,
      { type: { params: [], results: [] }, body: [...op.i32(3), ...op.i32(100), ...op.i32(0), ...op.call(0)], export: "init" },
      { type: { params: ["i32", "i32", "i32", "i32"], results: [] }, body: [], export: "__tick" },
    ],
  });
  let started = false;
  let error: unknown = null;
  try {
    await Program.load(wasm, checker(), new Recorder(), { startTicker: () => (started = true) });
  } catch (e) {
    error = e;
  }
  expect(error).toBeInstanceOf(TrapError);
  expect((error as Error).message).toBe(
    "the program trapped: it starts a ticker at 0 Hz; a ticker ticks at least once a second",
  );
  expect(started).toBe(false);
});

test("a rejected init starts no voice or ticker and tells spawned helpers to stop", async () => {
  const AUDIO: Import = {
    module: "wrela",
    name: "audio",
    kind: "func",
    type: { params: ["i32", "i32"], results: [] },
  };
  const TICK: Import = {
    module: "wrela",
    name: "tick",
    kind: "func",
    type: { params: ["i32", "i32", "i32"], results: [] },
  };
  const init = [
    ...op.i32(1),
    ...op.i32(2),
    ...op.call(0),
    ...op.i32(3),
    ...op.i32(4),
    ...op.i32(60),
    ...op.call(1),
    ...op.unreachable,
  ];
  const wasm = buildModule({
    imports: [AUDIO, TICK],
    memory: 20,
    funcs: [
      frameOnly,
      { type: { params: ["i32", "i32", "i32"], results: [] }, body: [], export: "__audio" },
      { type: { params: ["i32", "i32", "i32", "i32"], results: [] }, body: [], export: "__tick" },
      { type: { params: ["i32"], results: [] }, body: [], export: "__worker" },
      { type: { params: [], results: [] }, body: init, export: "init" },
    ],
  });
  let memory: WebAssembly.Memory | null = null;
  let voices = 0;
  let tickers = 0;
  await expect(
    Program.load(wasm, checker(), new Recorder(), {
      workers: 3,
      spawnWorker: (_module, shared) => {
        memory = shared;
      },
      startVoice: () => voices++,
      startTicker: () => tickers++,
    }),
  ).rejects.toThrow("the program trapped");
  expect(voices).toBe(0);
  expect(tickers).toBe(0);
  expect(memory).not.toBeNull();
  expect(new DataView(memory!.buffer).getUint32(PAR_SHUTDOWN, true)).toBe(1);
});

test("rejecting an empty-memory program preserves its initialization trap", async () => {
  const wasm = buildModule({
    memory: 0,
    funcs: [
      frameOnly,
      { type: { params: [], results: [] }, body: op.unreachable, export: "init" },
    ],
  });
  let error: unknown = null;
  try {
    await Program.load(wasm, checker(), new Recorder());
  } catch (e) {
    error = e;
  }
  expect(error).toBeInstanceOf(TrapError);
  expect((error as Error).message).toStartWith("the program trapped:");
});

test("another instance's start function cannot call its allowed host imports", async () => {
  const PRINT: Import = {
    module: "wrela",
    name: "print",
    kind: "func",
    type: { params: ["i32", "i32"], results: [] },
  };
  const wasm = buildModule({
    imports: [PRINT],
    memory: 1,
    funcs: [
      frameOnly,
      { type: { params: [], results: [] }, body: [...op.i32(0), ...op.i32(0), ...op.call(0)] },
    ],
    start: 2,
  });
  const module = new WebAssembly.Module(wasm);
  const memory = new WebAssembly.Memory({ initial: 1, maximum: 16384, shared: true });
  let printed = 0;
  await expect(otherInstance(module, memory, "a helper", () => printed++)).rejects.toThrow(
    "its start function called the host",
  );
  expect(printed).toBe(0);
});

test("catch_all cannot hide a start function's host call", async () => {
  const PRINT: Import = {
    module: "wrela",
    name: "print",
    kind: "func",
    type: { params: ["i32", "i32"], results: [] },
  };
  // try (result void); call host; catch_all; end. Cover one import another thread normally
  // answers and one it normally refuses: both must be remembered as start-time calls.
  const caught = [0x06, 0x40, ...op.i32(0), ...op.i32(0), ...op.call(0), 0x19, 0x0b];
  for (const host of [PRINT, SUBMIT]) {
    const wasm = buildModule({
      imports: [host],
      memory: 20,
      funcs: [frameOnly, { type: { params: [], results: [] }, body: caught }],
      start: 2,
    });
    await expect(Program.load(wasm, checker(), new Recorder())).rejects.toThrow(
      "its start function called the host",
    );

    const module = new WebAssembly.Module(wasm);
    const memory = new WebAssembly.Memory({ initial: 20, maximum: 16384, shared: true });
    await expect(otherInstance(module, memory, "a helper")).rejects.toThrow(
      "its start function called the host",
    );
  }
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

test("printed lines are strict UTF-8 and keep a byte-order mark", async () => {
  const PRINT: Import = { module: "wrela", name: "print", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  const program = async (bytes: Uint8Array) => {
    const lines: string[] = [];
    const p = await Program.load(
      buildModule({
        imports: [PRINT],
        memory: 1,
        funcs: [{ type: FRAME_TYPE, body: [...op.i32(16), ...op.i32(bytes.length), ...op.call(0)], export: "frame" }],
        data: [{ offset: 16, bytes }],
      }),
      checker(),
      new Recorder(),
      { onPrint: (line) => lines.push(line) },
    );
    return { p, lines };
  };
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    const marked = await program(Uint8Array.of(0xef, 0xbb, 0xbf, 0x78));
    marked.p.frame(0, 1, 1);
    expect(marked.lines).toEqual(["\uFEFFx"]);
    expect(log).toHaveBeenCalledWith("wrela: \uFEFFx");

    const malformed = await program(Uint8Array.of(0xff));
    expect(() => malformed.p.frame(0, 1, 1)).toThrow("print: the text isn't UTF-8");
    expect(malformed.lines).toEqual([]);
  } finally {
    log.mockRestore();
  }
});

test("a phase validates its memory range and UTF-8 name even though the browser doesn't time it", async () => {
  const PHASE: Import = { module: "wrela", name: "phase", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  const program = (ptr: number, len: number, data: Uint8Array[] = []) =>
    Program.load(
      buildModule({
        imports: [PHASE],
        memory: 1,
        funcs: [{ type: FRAME_TYPE, body: [...op.i32(ptr), ...op.i32(len), ...op.call(0)], export: "frame" }],
        data: data.map((bytes) => ({ offset: ptr, bytes })),
      }),
      checker(),
      new Recorder(),
    );
  const past = await program(-1, 1);
  expect(() => past.frame(0, 1, 1)).toThrow("phase(4294967295, 1) reaches past the end of the program's memory");
  const malformed = await program(16, 1, [Uint8Array.of(0xff)]);
  expect(() => malformed.frame(0, 1, 1)).toThrow("phase: the text isn't UTF-8");
});

test("another thread's printed line is relayed to its program", async () => {
  const PRINT: Import = { module: "wrela", name: "print", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  const line = new TextEncoder().encode("from helper");
  const wasm = buildModule({
    imports: [PRINT],
    memory: 1,
    funcs: [
      frameOnly,
      {
        type: { params: ["i32"], results: [] },
        body: [...op.i32(16), ...op.i32(line.length), ...op.call(0)],
        export: "__worker",
      },
    ],
    data: [{ offset: 16, bytes: line }],
  });
  const lines: string[] = [];
  let helper: {
    module: WebAssembly.Module;
    memory: WebAssembly.Memory;
    thread: number;
    onPrint: (line: string) => void;
  } | null = null;
  await Program.load(wasm, checker(), new Recorder(), {
    workers: 2,
    spawnWorker: (module, memory, thread, onPrint) => {
      helper = { module, memory, thread, onPrint };
    },
    onPrint: (printed) => lines.push(printed),
  });
  expect(helper).not.toBeNull();
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    const { module, memory, thread, onPrint } = helper!;
    const instance = await otherInstance(module, memory, "a helper", onPrint);
    (instance.exports.__worker as (thread: number) => void)(thread);
    expect(lines).toEqual(["from helper"]);
    expect(log).toHaveBeenCalledWith("wrela: from helper");
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
    const batch = new Encoder().post(4, "notes/reply", Uint8Array.of(1, 2, 3, 4)).finish();
    const io = {
      fetch: async () => new Uint8Array(0),
      storageRead: async () => new Uint8Array(0),
      storageWrite: async () => {},
      post: async (url: string, body: Uint8Array) => {
        expect(url).toBe("notes/reply");
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
  const PAYLOAD = { module: "wrela", name: "payload", kind: "func" as const, type: { params: ["i32" as const, "i32" as const, "i32" as const, "i32" as const], results: ["i32" as const] } };
  // `read(cap)` takes one event to 1024 and reads its message's bytes, up to `cap`, to 2048, in
  // the same call; `again()` reads the same serial in a call of its own.
  const serial = [...op.i32(1036), 0x28, 0x02, 0x00, ...op.i32(1040), 0x28, 0x02, 0x00];
  const messages = buildModule({
    imports: [INPUT, PAYLOAD],
    memory: 1,
    funcs: [
      { type: FRAME_TYPE, body: [], export: "frame" },
      { type: { params: ["i32"], results: ["i32"] }, body: [...op.i32(1024), ...op.i32(1), ...op.call(0), 0x1a, ...serial, ...op.i32(2048), 0x20, 0x00, ...op.call(1)], export: "read" },
      { type: { params: [], results: ["i32"] }, body: [...serial, ...op.i32(2048), ...op.i32(64), ...op.call(1)], export: "again" },
      { type: { params: ["i32"], results: ["i32"] }, body: [0x20, 0x00, 0x28, 0x02, 0x00], export: "word" },
    ],
  });

  test("a message's bytes are read in the call given it, and no later", async () => {
    const p = await Program.load(messages, checker(), new Recorder());
    p.inbox.pushMessage(3, Uint8Array.of(9, 8, 7, 6, 5));
    // Its length, whatever it copied.
    expect(p.call("read", 2)).toBe(5);
    expect(p.call("word", 1024)).toBe(13);
    expect(p.call("word", 1028 + 4)).toBe(3);
    // Two bytes copied, and no more.
    expect(p.call("word", 2048)).toBe(0x0809);
    expect(p.call("again")).toBe(-1);
  });

  test("a call is given messages up to a budget of bytes, the first whatever its size", async () => {
    const { PAYLOAD_PER_CALL } = await import("../src/abi.gen.ts");
    const { Inbox } = await import("../src/net.ts");
    const inbox = new Inbox();
    inbox.pushMessage(1, new Uint8Array(PAYLOAD_PER_CALL + 1));
    inbox.pushMessage(1, new Uint8Array(1));
    expect(inbox.take(8).length).toBe(1);
    inbox.endCall();
    expect(inbox.take(8).length).toBe(1);
  });
});
