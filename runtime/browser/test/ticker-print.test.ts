import { expect, spyOn, test } from "bun:test";
import { TICK_HASH } from "../src/abi.gen.ts";
import { VisibleClock } from "../src/frame.ts";
import { InputRing } from "../src/input.ts";
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
