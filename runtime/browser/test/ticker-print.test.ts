import { expect, spyOn, test } from "bun:test";
import { EVENT_SIZE, TICK_HASH, TICK_ORIGIN, TICK_RECORDS } from "../src/abi.gen.ts";
import { VisibleClock } from "../src/frame.ts";
import { InputRing, textEvent } from "../src/input.ts";
import type { FromTicker, TickerStart } from "../src/messages.ts";
import { runTicker, TickerControl } from "../src/ticker.ts";
import { buildModule, type Import, op } from "./wasm-builder.ts";

test("the ticker relays prints without mistaking them for its final report", async () => {
  const PRINT: Import = { module: "wrela", name: "print", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  const line = new TextEncoder().encode("from ticker");
  const module = await WebAssembly.compile(
    buildModule({
      imports: [PRINT],
      memory: 20,
      funcs: [
        {
          type: { params: ["i32", "i32", "i32", "i32"], results: [] },
          body: [...op.i32(16), ...op.i32(line.length), ...op.call(0), ...op.unreachable],
          export: "__tick",
        },
      ],
      data: [{ offset: 16, bytes: line }],
    }),
  );
  const memory = new WebAssembly.Memory({ initial: 20, maximum: 16384, shared: true });
  new DataView(memory.buffer).setBigUint64(TICK_HASH, 0n, true);
  const control = TickerControl.create();
  control.go(2); // lockstep
  control.runTo(1);
  const start: TickerStart = {
    module,
    memory,
    task: 0,
    context: 0,
    hz: 60,
    control: control.buffer,
    clock: VisibleClock.create().buffer,
    input: InputRing.create().buffer,
    script: [],
    hashes: false,
    timing: false,
    wasmHash: 0n,
    delay: 0,
    replayable: false,
    replay: null,
    framesBeforeStart: 0,
  };
  const messages: FromTicker[] = [];
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    await runTicker(start, (message) => messages.push(message));
    expect(messages.map((message) => message.type)).toEqual(["print", "fatal"]);
    expect(messages[0]).toEqual({ type: "print", line: "from ticker" });
    expect(log).toHaveBeenCalledWith("wrela: from ticker");
  } finally {
    log.mockRestore();
  }
});

test("the ticker control's counters cross the signed boundary as unsigned words", async () => {
  const control = TickerControl.create();
  const n = 0x8000_0000;
  control.framesStarted(n);
  control.runTo(n);
  control.ran(n);
  expect(control.frames).toBe(n);
  expect(control.target).toBe(n);
  expect(await Promise.race([control.ranTo(n).then(() => "resolved"), Bun.sleep(20).then(() => "timed out")])).toBe("resolved");
});

test("a ticker failure cannot miss a frame about to wait for its ticks", async () => {
  const control = TickerControl.create();
  const waitAsync = Atomics.waitAsync;
  const failBeforeWait = ((words: Int32Array, index: number, value: number, timeout?: number) => {
    // The ticker traps after ranTo checks its state, just before the frame begins waiting.
    control.fail();
    return waitAsync(words, index, value, timeout);
  }) as typeof Atomics.waitAsync;
  const wait = spyOn(Atomics, "waitAsync").mockImplementation(failBeforeWait);
  try {
    expect(await Promise.race([control.ranTo(1).then(() => "resolved"), Bun.sleep(20).then(() => "timed out")])).toBe("resolved");
    expect(control.failed).toBe(true);
  } finally {
    wait.mockRestore();
    control.ran(1);
  }
});

test("stopping a ticker releases a frame waiting for ticks", async () => {
  const control = TickerControl.create();
  const waiting = control.ranTo(1);
  control.stop();
  try {
    expect(await Promise.race([waiting.then(() => "resolved"), Bun.sleep(20).then(() => "timed out")])).toBe("resolved");
    // A tick that was already running may publish its count after stop; later waits still end.
    control.ran(1);
    expect(await Promise.race([control.ranTo(2).then(() => "resolved"), Bun.sleep(20).then(() => "timed out")])).toBe("resolved");
  } finally {
    control.ran(2);
  }
});

test("lockstep ticks publish the build origin and preserve the replay origin", async () => {
  const module = await WebAssembly.compile(buildModule({ memory: 20, funcs: [{
    type: { params: ["i32", "i32", "i32", "i32"], results: [] }, body: op.unreachable, export: "__tick",
  }] }));
  for (const origin of [null, 0.5]) {
    const memory = new WebAssembly.Memory({ initial: 20, maximum: 16384, shared: true });
    const control = TickerControl.create();
    control.go(2, 1.5);
    control.runTo(1);
    const messages: FromTicker[] = [];
    await runTicker({
      module, memory, task: 0, context: 0, hz: 60, control: control.buffer,
      clock: VisibleClock.create().buffer, input: InputRing.create().buffer,
      script: [], hashes: false, timing: false, wasmHash: 0n, delay: 0, replayable: false,
      replay: origin === null ? null : { upto: 1, ticks: [], origin },
      framesBeforeStart: 0,
    }, message => messages.push(message));
    expect(messages.map(message => message.type)).toEqual(["fatal"]);
    expect(new DataView(memory.buffer).getFloat64(TICK_ORIGIN, true)).toBe(origin ?? 1.5);
  }
});

test("a ticker skips scripts before its start and keeps frames that start while it loads", async () => {
  const module = await WebAssembly.compile(buildModule({ memory: 20, funcs: [{
    type: { params: ["i32", "i32", "i32", "i32"], results: [] }, body: op.unreachable, export: "__tick",
  }] }));
  for (const replay of [null, { upto: 0, ticks: [], origin: null }]) {
    const memory = new WebAssembly.Memory({ initial: 20, maximum: 16384, shared: true });
    const control = TickerControl.create();
    control.framesStarted(4);
    control.go(2);
    control.runTo(1);
    await runTicker({
      module, memory, task: 0, context: 0, hz: 60, control: control.buffer,
      clock: VisibleClock.create().buffer, input: InputRing.create().buffer,
      script: [0, 1, 2, 3].map(frame => ({ at: { frame }, event: textEvent(65 + frame) })),
      hashes: false, timing: false, wasmHash: 0n, delay: 0, replayable: false, replay,
      framesBeforeStart: 2,
    }, () => {});
    expect(new DataView(memory.buffer).getUint32(TICK_RECORDS, true)).toBe(2);
    expect(new Uint8Array(memory.buffer, TICK_RECORDS + 4, 2 * EVENT_SIZE)).toEqual(new Uint8Array([
      ...textEvent(67), ...textEvent(68),
    ]));
  }
});
