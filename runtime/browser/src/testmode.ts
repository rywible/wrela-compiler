// Test mode: `index.html#test&frames=60&width=1920&height=1080&fps=60&workers=4&audio=750` runs a
// fixed number of frames at fixed times and canvas size, its parallel jobs on a fixed number of
// threads (the program's own included), then saves the last frame, the state hash, the lines
// the program printed (`log.txt`) and each frame's CPU time, when it began, and the frame each
// printed line came in (`frames.json`) to `results/` for
// tools/headless.py. With `audio`, the main thread also renders that many
// quanta of the program's voice offline, in an AudioWorklet, and saves the samples; with
// `timestamps=1`, each pass's GPU time, start and end (`timings.json`, where the device has
// timestamp queries); with `timestamps=2`, each pass and dispatch run alone, one at a time, so each
// time is its own (on Apple GPUs passes overlap);
// with `nohash=1`, no state hash (hashing every submitted byte costs CPU time a timing run
// shouldn't count; `hash.txt` then says `none`);
// with `input=script.json`, a script of input events (runtime/abi `input`), each queued before
// the frame it's for; with `latency=n`, the main thread sends n pointer events through the DOM at
// random times while the frames run in real time, and saves when each reached the program
// (`latency.json`); with `keylatency=n`, n presses of the right arrow key instead, at least
// 250 ms apart.
//
// A program with a ticker (`std::tick::start`) runs it on the lockstep schedule (#43 §2.3):
// before frame i, the ticks up to ⌊(i + 1)·hz/fps⌋ run, and the frame waits for them, as on
// the native host. With `paced=1`, ticks run on the ticker's own clock instead and neither waits
// for the other, as in normal play: for time budgets. Either way, `ticks.json` has each tick's
// CPU time, and, unless `nohash=1`, `ticks.log` is the tick log (runtime/abi `ticks`: each
// tick's records and state hash), which `wrela-host --replay` replays. With `tickdelay=ms`, each tick is
// held that much longer; with `framedelay=ms`, each frame. With `saturate=1`, each frame starts
// as soon as the last is recorded, not at its time, with at most two frames on the GPU and the
// canvas left alone (it would pace them by the display): a GPU that sets its clock by its load
// stays busy, so a frame's GPU time is its work (GPU budgets). With `salt=n`, each shader gets a
// comment that makes it unique, so no cache serves its pipelines, and `pipelines.json` has how
// long creating them all, at once, took (#42 AC4's cold pipelines). `load.json` has when the page opened
// and each file it loaded; `memory.json` the bytes of the program's GPU buffers and textures (at
// the end, and at most) and of its WASM memory (reserved, and grown to). It's part of the shipped bundle, so the agreement test runs
// the exact bytes a game ships, but only a page served from this machine (tools/serve.py and
// tools/headless.py bind 127.0.0.1) enters it: a game's public URL ignores `#test`.

import type { Loaded } from "./messages.ts";

export interface TestParams {
  frames: number;
  width: number;
  height: number;
  fps: number;
  workers: number;
  /** Quanta of the voice to render (0: none). */
  audio: number;
  /** 1: time each pass on the GPU. */
  timestamps: number;
  /** 1: keep no state hash. */
  nohash: number;
  /** A script of input events, relative to the page; "" for none. */
  input: string;
  /** Pointer events to send through the DOM while the frames run (0: none). */
  latency: number;
  /** Key presses to send through the DOM while the frames run (0: none). */
  keylatency: number;
  /** 1: ticks on the ticker's own clock, not in lockstep with the frames. */
  paced: number;
  /** Ms each tick is held longer (0: none). */
  tickdelay: number;
  /** Ms each frame is held longer (0: none). */
  framedelay: number;
  /** A number that makes every shader's source unique (0: none). */
  salt: number;
  /** 1: each frame as soon as the last is done, not at its time. */
  saturate: number;
}

export const TEST_DEFAULTS: TestParams = {
  frames: 60,
  width: 1920,
  height: 1080,
  fps: 60,
  workers: 1,
  audio: 0,
  timestamps: 0,
  nohash: 0,
  input: "",
  latency: 0,
  keylatency: 0,
  paced: 0,
  tickdelay: 0,
  framedelay: 0,
  salt: 0,
  saturate: 0,
};
/** Every parameter's name, in order. */
const NAMES = Object.keys(TEST_DEFAULTS) as (keyof TestParams)[];
/** The parameters that are numbers. */
type Count = { [K in keyof TestParams]: TestParams[K] extends number ? K : never }[keyof TestParams];
const KEYS = NAMES.filter((k): k is Count => typeof TEST_DEFAULTS[k] === "number");

/** Whether a page's host is this machine, where test mode may run. */
export function isLoopback(hostname: string): boolean {
  return hostname === "localhost" || hostname === "127.0.0.1" || hostname === "[::1]" || hostname === "::1";
}

/** Whether a URL fragment asks for test mode, whether or not its parameters are valid. */
export const asksForTest = (hash: string) => hash.replace(/^#/, "").split("&")[0] === "test";

/** Test mode's parameters from a URL fragment, or null if the fragment doesn't ask for it. */
export function parseTestParams(hash: string): TestParams | null {
  if (!asksForTest(hash)) return null;
  const [, ...pairs] = hash.replace(/^#/, "").split("&");
  const params = { ...TEST_DEFAULTS };
  for (const pair of pairs) {
    const [key, value = ""] = pair.split("=", 2);
    if (key === "input") {
      if (value === "" || value.startsWith("/") || value.includes(":") || value.split("/").some((p) => p === "" || p === "." || p === "..")) {
        throw new Error(`test parameter input=${value} must be a path relative to the page`);
      }
      params.input = value;
      continue;
    }
    if (!KEYS.some((k) => k === key)) {
      const expected = `${NAMES.slice(0, -1).join(", ")} or ${NAMES[NAMES.length - 1]}`;
      throw new Error(`unknown test parameter \`${key}\` (expected ${expected})`);
    }
    const n = Number(value);
    const ok = key === "fps" ? Number.isFinite(n) && n > 0 : Number.isInteger(n) && n > 0 && /^\d+$/.test(value);
    if (!ok) throw new Error(`test parameter ${key}=${value} must be a positive ${key === "fps" ? "number" : "integer"}`);
    params[key as Count] = n;
  }
  return params;
}

/** The time of frame `i`, as both hosts compute it (runtime/abi `ticks`' `frame_time`, which the
 * vectors check): `i / fps` (the WASM call rounds it to f32). */
export const frameTime = (i: number, fps: number) => i / fps;

/** PUTs a file into the page's `results/` directory (see tools/serve.py). */
export async function putResult(base: string, name: string, body: BodyInit): Promise<void> {
  const url = new URL(`results/${name}`, base);
  const response = await fetch(url, { method: "PUT", body });
  if (!response.ok) throw new Error(`PUT ${url} failed: ${response.status} ${response.statusText}`);
}

/** What this thread loaded, for `load.json`: its performance entries of `types` ("navigation",
 * "resource"), in that order. */
export function loaded(types: string[]): Loaded[] {
  return types
    .flatMap((type) => performance.getEntriesByType(type) as PerformanceResourceTiming[])
    .map((e) => ({
      name: e.name,
      bytes: e.transferSize > 0 ? e.transferSize : e.encodedBodySize,
      end_ms: performance.timeOrigin + e.responseEnd,
    }));
}

/** Resolves at `ms` (`performance.now()`'s clock), or at once if that's past. */
export async function sleepUntil(ms: number): Promise<void> {
  const wait = ms - performance.now();
  if (wait > 0) await new Promise((resolve) => setTimeout(resolve, wait));
}

/** Blocks this thread for `ms`, as if its work took that long (`tickdelay`, `framedelay`). */
export function holdFor(ms: number): void {
  // Nothing wakes this word.
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}
