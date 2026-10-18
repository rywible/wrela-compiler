import { afterEach, expect, spyOn, test } from "bun:test";
import { AUDIO_OUT, AUDIO_QUANTUM } from "../src/abi.gen.ts";
import type { FromVoice, VoiceProcessorOptions } from "../src/messages.ts";
import { buildModule, type Import, op, SUBMIT } from "./wasm-builder.ts";

const oldProcessor = Object.getOwnPropertyDescriptor(globalThis, "AudioWorkletProcessor");
const oldRegister = Object.getOwnPropertyDescriptor(globalThis, "registerProcessor");
const oldDecoder = Object.getOwnPropertyDescriptor(globalThis, "TextDecoder");

afterEach(() => {
  if (oldProcessor) Object.defineProperty(globalThis, "AudioWorkletProcessor", oldProcessor);
  else delete (globalThis as Record<string, unknown>).AudioWorkletProcessor;
  if (oldRegister) Object.defineProperty(globalThis, "registerProcessor", oldRegister);
  else delete (globalThis as Record<string, unknown>).registerProcessor;
  if (oldDecoder) Object.defineProperty(globalThis, "TextDecoder", oldDecoder);
  else delete (globalThis as Record<string, unknown>).TextDecoder;
});

test.each(["with", "without"] as const)("the audio worklet %s TextDecoder discriminates a printed line from a trap", async (decoder) => {
  const messages: FromVoice[] = [];
  class Processor {
    readonly port = { postMessage: (message: FromVoice) => messages.push(message) };
  }
  type VoiceConstructor = new (options: { processorOptions: VoiceProcessorOptions }) => {
    process(inputs: Float32Array[][], outputs: Float32Array[][]): boolean;
  };
  let Voice: VoiceConstructor | null = null;
  Object.defineProperty(globalThis, "AudioWorkletProcessor", { value: Processor, configurable: true });
  Object.defineProperty(globalThis, "registerProcessor", {
    value: (_name: string, processor: VoiceConstructor) => {
      Voice = processor;
    },
    configurable: true,
  });
  if (decoder === "without") {
    const bundle = await Bun.build({ entrypoints: [new URL("../src/worklet.ts", import.meta.url).pathname], target: "browser", format: "esm" });
    expect(bundle.success).toBe(true);
    const path = `data:text/javascript;base64,${Buffer.from(await bundle.outputs[0]!.text()).toString("base64")}`;
    Reflect.deleteProperty(globalThis, "TextDecoder");
    await import(path);
  } else await import(`../src/worklet.ts?print=${Date.now()}`);

  const PRINT: Import = { module: "wrela", name: "print", kind: "func", type: { params: ["i32", "i32"], results: [] } };
  const line = new TextEncoder().encode("from voice");
  const module = await WebAssembly.compile(
    buildModule({
      imports: [PRINT],
      memory: 20,
      funcs: [
        {
          type: { params: ["i32", "i32", "i32"], results: [] },
          body: [...op.i32(16), ...op.i32(line.length), ...op.call(0)],
          export: "__audio",
        },
      ],
      data: [{ offset: 16, bytes: line }],
    }),
  );
  const memory = new WebAssembly.Memory({ initial: 20, maximum: 16384, shared: true });
  const log = spyOn(console, "log").mockImplementation(() => {});
  try {
    const VoiceClass = Voice as unknown as VoiceConstructor;
    const voice = new VoiceClass({ processorOptions: { module, memory, task: 0, context: 0, quanta: 1 } });
    const output = new Float32Array(AUDIO_QUANTUM);
    expect(voice.process([], [[output]])).toBe(true);
    expect(messages).toEqual([{ type: "print", line: "from voice" }, { type: "rendered" }]);
    expect(log).toHaveBeenCalledWith("wrela: from voice");
    expect(new Float32Array(memory.buffer, AUDIO_OUT, AUDIO_QUANTUM)).toEqual(output);
    const caught = await WebAssembly.compile(buildModule({ imports: [SUBMIT], memory: 20, funcs: [{
      type: { params: ["i32", "i32", "i32"], results: [] }, export: "__audio",
      body: [0x06, 0x40, ...op.i32(0), ...op.i32(0), ...op.call(0), 0x19, 0x0b],
    }] }));
    messages.length = 0;
    const refused = new VoiceClass({ processorOptions: { module: caught, memory, task: 0, context: 0, quanta: 1 } });
    output.fill(9);
    expect(refused.process([], [[output]])).toBe(false);
    expect(messages).toEqual([{ type: "trap", message: "the audio thread can't call the host" }]);
    expect(output).toEqual(new Float32Array(AUDIO_QUANTUM).fill(9));
  } finally {
    log.mockRestore();
  }
});
