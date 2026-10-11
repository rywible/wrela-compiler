// Messages between the threads: the main thread (main.ts), the render worker (worker.ts), and the
// threads the render worker starts (the ticker's, ticker.ts, and the helpers).

import type { Scripted } from "./input.ts";
import type { TestParams } from "./testmode.ts";

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
      /** The page is served by `wrela run`: new literals and new builds come while it runs
       * (hot reload, live.ts), after the change numbered here, the last its build holds; null
       * otherwise. */
      live: number | null;
    }
  | { type: "resize"; width: number; height: number }
  | { type: "visibility"; visible: boolean }
  /** To a worker thread the render worker starts: run the program's jobs (`runWorker`). */
  | { type: "thread"; module: WebAssembly.Module; memory: WebAssembly.Memory; index: number }
  /** Test mode: the main thread has rendered the voice and saved its samples. */
  | { type: "audio-rendered" }
  /** Test mode, `latency` or `keylatency`: the main thread has sent its events. */
  | { type: "latency-sent" }
  /** Test mode, `clip`: the main thread has saved the clip (`clip.webm`). */
  | { type: "clip-saved" }
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
  /** Test mode, `clip`: start recording the canvas, or stop and save the recording. */
  | { type: "clip-start" }
  | { type: "clip-stop" }
  /** The program started its voice: the main thread plays it in an AudioWorklet. */
  | { type: "audio"; voice: VoiceOptions }
  /** Hot reload of a program whose voice plays: the page reloads. */
  | { type: "reload" };

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

/** The program's ticker, as its thread gets it: the module and memory to run it with, and the
 * task, context and rate `__tick` takes (wrela_abi's `IMPORT_TICK`). */
export interface TickerOptions {
  module: WebAssembly.Module;
  memory: WebAssembly.Memory;
  task: number;
  context: number;
  hz: number;
}

/** What the ticker's thread is given (the `ticker` message): the program's ticker, and how to
 * run it. */
export interface TickerStart extends TickerOptions {
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
  /** The page is live (hot reload): keep each tick's records, for a build that replaces this
   * one to run them again. */
  replayable: boolean;
  /** Hot reload: the ticks the replaced build ran, to run again first, and its clock's origin. */
  replay: Replay | null;
}

/** The ticks a replaced build ran (hot reload): how many, the records of those that had any,
 * and the origin of its clock (std::tick's `origin`), which the new ticker keeps. */
export interface Replay {
  upto: number;
  ticks: [number, Uint8Array<ArrayBuffer>][];
  origin: number | null;
}

/** What the ticks did, for test mode's results (`ticks.json`; their records and state hashes
 * are in the tick log, `ticks.log`). */
export interface TickReport {
  /** How long each tick took (ms). */
  cpu_ms: number[];
}

/** What the ticker's thread tells the render worker. */
export type FromTicker =
  | { type: "fatal"; message: string }
  /** Stopped (test mode, or a hot reload): the ticks, the tick log when hashes were kept, and
   * what a build that replaces this one replays. */
  | { type: "ticks"; report: TickReport; log: Uint8Array<ArrayBuffer> | null; replay: Replay };
