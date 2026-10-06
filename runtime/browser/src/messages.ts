// Messages between the main thread (main.ts) and the render worker (worker.ts).

import type { TestParams } from "./testmode.ts";
import type { Scripted } from "./input.ts";

export type ToWorker =
  | {
      type: "start";
      canvas: OffscreenCanvas;
      /** The page's URL: the build's files and `results/` resolve against it. */
      base: string;
      /** The canvas's size in device pixels. */
      width: number;
      height: number;
      /** Test mode's parameters, or null to run normally. */
      test: TestParams | null;
      /** The ring the main thread writes input events into (input.ts). */
      input: SharedArrayBuffer;
    }
  | { type: "resize"; width: number; height: number }
  | { type: "visibility"; visible: boolean }
  /** To a worker thread the render worker starts: run the program's jobs (`runWorker`). */
  | { type: "thread"; module: WebAssembly.Module; memory: WebAssembly.Memory; index: number }
  /** Test mode: the main thread has rendered the voice and saved its samples. */
  | { type: "audio-rendered" }
  /** Test mode, `latency` or `keylatency`: the main thread has sent its events. */
  | { type: "latency-sent" }
  /** To the ticker's thread the render worker starts: run the program's ticks (ticker.ts). */
  | ({ type: "ticker" } & TickerStart)
  /** Test mode: what the page loaded, for `load.json`. */
  | { type: "load"; opened_ms: number; resources: Loaded[] };

/** A file the page loaded: its URL, its bytes over the network (or its encoded size, when the
 * transfer size isn't known), and when it finished (ms since 1970). */
export interface Loaded {
  name: string;
  bytes: number;
  end_ms: number;
}

export type FromWorker =
  | { type: "fatal"; message: string }
  /** Test mode, `latency` or `keylatency`: the frames have started; send the events over `ms`
   * milliseconds. */
  | { type: "latency-start"; ms: number }
  /** Test mode: the main thread answers with what the page loaded (`load`). */
  | { type: "load-query" }
  /** The program started its voice: the main thread plays it in an AudioWorklet. */
  | { type: "audio"; voice: VoiceOptions };

/** The program's voice, as the audio worklet's processor gets it: the module and memory to
 * run it with, and the task and context `__audio` takes (wrela_abi's `IMPORT_AUDIO`). */
export interface VoiceOptions {
  module: WebAssembly.Module;
  memory: WebAssembly.Memory;
  task: number;
  context: number;
}

/** The audio worklet's processor (worklet.ts). */
export const VOICE_PROCESSOR = "wrela-voice";

/** What the ticker's thread is given (the `ticker` message). */
export interface TickerStart {
  module: WebAssembly.Module;
  memory: WebAssembly.Memory;
  task: number;
  context: number;
  hz: number;
  control: SharedArrayBuffer;
  clock: SharedArrayBuffer;
  /** The input ring (input.ts), whose ticker reader the render worker has attached. */
  input: SharedArrayBuffer;
  /** Test mode: a script's events; frame-keyed ones are the ticker's once their frame starts. */
  script: Scripted[];
  /** Test mode: keep each tick's records and state hash, for a tick log. */
  hashes: boolean;
  /** Test mode: keep each tick's time. */
  timing: boolean;
  /** The build's WASM hash, for the tick log's header. */
  wasmHash: bigint;
  /** Test mode: each tick is held this many ms more, as if it took that long. */
  delay: number;
}

