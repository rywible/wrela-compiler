// The audio worklet: renders the program's voice on the audio thread, one render quantum per
// `process` call, by calling wrela_abi's `__audio` (on the audio thread, wrela_abi memory's
// `THREAD_AUDIO`) on its own instance of the program's module,
// which shares the program's memory. The samples are at `AUDIO_OUT` after each call.

import { AUDIO_OUT, AUDIO_QUANTUM, EXPORT_AUDIO, HOST_FUNCTIONS, IMPORT_MEMORY, IMPORT_MODULE, THREAD_AUDIO } from "./abi.gen.ts";
import { VOICE_PROCESSOR, type VoiceOptions } from "./messages.ts";

// The AudioWorkletGlobalScope's own names, which TypeScript has no lib for.
declare class AudioWorkletProcessor {
  readonly port: { postMessage(message: unknown): void };
}
declare function registerProcessor(
  name: string,
  processor: new (options: { processorOptions: VoiceOptions }) => AudioWorkletProcessor,
): void;

class Voice extends AudioWorkletProcessor {
  readonly #render: (thread: number, task: number, context: number) => void;
  readonly #samples: Float32Array;
  readonly #task: number;
  readonly #context: number;
  #failed = false;

  constructor(options: { processorOptions: VoiceOptions }) {
    super();
    const { module, memory, task, context } = options.processorOptions;
    // The voice's code records no GPU work and makes no requests (its effects are checked).
    const refuse = () => {
      throw new Error("the audio thread can't call the host");
    };
    const imports = {
      [IMPORT_MODULE]: { [IMPORT_MEMORY]: memory, ...Object.fromEntries(HOST_FUNCTIONS.map(([name]) => [name, refuse])) },
    };
    const instance = new WebAssembly.Instance(module, imports);
    this.#render = instance.exports[EXPORT_AUDIO] as (thread: number, task: number, context: number) => void;
    // The memory may grow, but a view of shared memory stays valid, and this one is below the heap.
    this.#samples = new Float32Array(memory.buffer, AUDIO_OUT, AUDIO_QUANTUM);
    this.#task = task;
    this.#context = context;
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    if (this.#failed) return false;
    try {
      this.#render(THREAD_AUDIO, this.#task, this.#context);
    } catch (e) {
      // A trap stops the voice: say so, and render silence from here on.
      this.#failed = true;
      this.port.postMessage({ type: "trap", message: e instanceof Error ? e.message : String(e) });
      return false;
    }
    outputs[0]?.[0]?.set(this.#samples);
    return true;
  }
}

registerProcessor(VOICE_PROCESSOR, Voice);
