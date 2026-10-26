// Messages between the threads: the main thread (main.ts), the render worker (worker.ts), and the
// threads the render worker asks the main thread to start (the ticker's, ticker.ts, and the
// helpers), which it then speaks to through a channel of each one's own.

import type { Scripted } from "./input.ts";
import type { TestParams } from "./testmode.ts";

export type ToWorker =
  | {
      type: "start";
      canvas: OffscreenCanvas;
      /** The page's URL: the build's files and `results/` resolve against it. */
      base: string;
      /** Stable logical storage namespace, independent of immutable build URLs. */
      storage: string | null;
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
      /** The build's files the page is fetching for the worker (loader.ts `Handed`): each comes
       * in a `file` message. */
      given: string[];
    }
  /** A file of `given`: its bytes, or null if the page couldn't fetch it. */
  | { type: "file"; url: string; bytes: Uint8Array<ArrayBuffer> | null }
  | { type: "resize"; width: number; height: number }
  | { type: "visibility"; visible: boolean }
  | { type: "host-reply"; id: number; bytes: Uint8Array<ArrayBuffer>; error: string | null }
  /** To a thread the main thread started for the render worker: its channel to the render
   * worker, on which its work comes and its answers go. */
  | { type: "parent"; port: MessagePort }
  /** A thread the render worker asked for failed, or couldn't start. */
  | { type: "thread-failed"; id: number; message: string }
  /** To a worker thread the render worker starts: run the program's jobs (`runWorker`). */
  | { type: "thread"; module: WebAssembly.Module; memory: WebAssembly.Memory; index: number }
  /** Test mode: the main thread has rendered the voice and saved its samples. */
  | { type: "audio-rendered" }
  /** The audio thread trapped: the render worker stops the program and reports the failure. */
  | { type: "audio-failed"; message: string }
  /** A line the audio thread printed, relayed by the main thread. */
  | PrintMessage
  /** Test mode, `latency` or `keylatency`: the main thread has sent its events. */
  | { type: "latency-sent" }
  /** Test mode, `clip`: the main thread has saved the clip (`clip.webm`). */
  | { type: "clip-saved" }
  | { type: "summary-shown" }
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
  /** The largest screen dimension, so input and semantic geometry use the frame's fitted size. */
  | { type: "screen-limit"; max: number }
  /** `unsupported`: the machine can't run a program (`UnsupportedError`), rather than it failed. */
  | { type: "fatal"; message: string; unsupported: boolean }
  | { type: "host-request"; id: number; service: string; bytes: Uint8Array<ArrayBuffer> }
  | { type: "presented" }
  /** Start a thread, this script again, named `name`, and hand it `port` (a `parent` message):
   * a worker can't start one in every browser (the Claude app's browser pane can't). */
  | { type: "spawn"; id: number; name: string; port: MessagePort }
  /** Stop thread `id`. */
  | { type: "stop-thread"; id: number }
  /** Test mode, `latency` or `keylatency`: the frames have started; send the events over `ms`
   * milliseconds. */
  | { type: "latency-start"; ms: number }
  /** Test mode: the main thread answers with what the page loaded (`load`). */
  | { type: "load-query" }
  /** Test mode, `show=1`: the run's summary, to draw over the canvas. */
  | { type: "summary"; lines: string[] }
  /** Test mode, `clip`: start recording the canvas, or stop and save the recording. */
  | { type: "clip-start" }
  | { type: "clip-stop" }
  /** The program started its voice: the main thread plays it in an AudioWorklet. */
  | { type: "audio"; voice: VoiceOptions }
  /** Hot reload of a program whose voice plays: the page reloads. */
  | { type: "reload" }
  /** The program whose voice plays handed its place to another build: the voice stops (the
   * new build's voice, if it starts one, plays in the same audio context). */
  | { type: "audio-stop" };

/** The program's voice, as the audio worklet's processor gets it: the module and memory to
 * run it with, and the task and context `__audio` takes (wrela_abi's `IMPORT_AUDIO`). */
export interface VoiceOptions {
  module: WebAssembly.Module;
  memory: WebAssembly.Memory;
  task: number;
  context: number;
}

/** What the audio processor receives: in an offline test, how many quanta it renders. */
export interface VoiceProcessorOptions extends VoiceOptions {
  quanta: number | null;
}

/** The audio worklet's processor (worklet.ts). */
export const VOICE_PROCESSOR = "wrela-voice";

/** A line one of the program's other threads printed, relayed to its render thread. */
export interface PrintMessage {
  type: "print";
  line: string;
}

/** What the audio worklet tells the main thread. */
export type FromVoice = PrintMessage | { type: "trap"; message: string } | { type: "rendered" };

/** What a helper tells the render worker. */
export type FromHelper = PrintMessage;

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
  /** Frames already started when the ticker was requested: their scripts preceded this ticker. */
  framesBeforeStart: number;
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
  | PrintMessage
  /** Stopped (test mode, or a hot reload): the ticks, the tick log when hashes were kept, and
   * what a build that replaces this one replays. */
  | { type: "ticks"; report: TickReport; log: Uint8Array<ArrayBuffer> | null; replay: Replay };
