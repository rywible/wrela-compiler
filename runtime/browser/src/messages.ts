// Messages between the main thread (main.ts) and the render worker (worker.ts).

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
    }
  | { type: "resize"; width: number; height: number }
  | { type: "visibility"; visible: boolean }
  /** To a worker thread the render worker starts: run the program's jobs (`runWorker`). */
  | { type: "thread"; module: WebAssembly.Module; memory: WebAssembly.Memory; index: number }
  /** Test mode: the main thread has rendered the voice and saved its samples. */
  | { type: "audio-rendered" }
  /** Test mode, `latency`: the main thread has sent its events. */
  | { type: "latency-sent" };

export type FromWorker =
  | { type: "fatal"; message: string }
  /** Test mode, `latency`: the frames have started; send the events over `ms` milliseconds. */
  | { type: "latency-start"; ms: number }
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
