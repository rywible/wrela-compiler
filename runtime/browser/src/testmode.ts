// Test mode: `index.html#test&frames=60&width=1920&height=1080&fps=60&workers=4&audio=750` runs a
// fixed number of frames at fixed times and canvas size, its parallel jobs on a fixed number of
// threads (the program's own included), then saves the last frame, the state hash, the lines
// the program printed (`log.txt`), each frame's CPU time, when it began, the phases it timed
// (`std::time::phase`, ms by name) and the frame each printed line came in (`frames.json`), and
// what the executor did each frame below the stream (`counters.json`: passes, dispatches, draws
// and draw calls, buffer and texture writes, bind groups and pipelines set), and the adapter and
// the features the program saw (`device.json`) to `results/` for
// whoever runs the page (the compiler's
// tests' `wrela_host::pages`, a tool's browser). With `audio`, the main thread also renders that many
// quanta of the program's voice offline, in an AudioWorklet, and saves the samples (and how long
// the render took, `audio-ms.json`); with `live=s`, it plays the voice in real time for s seconds
// instead and saves what the browser says of the playback (`live.json`: its underruns, where it
// reports them); with
// `timestamps=1`, each pass's GPU time, start and end (`timings.json`, where the device has
// timestamp queries); with `timestamps=2`, each pass and dispatch run alone, one at a time, so each
// time is its own (on Apple GPUs passes overlap);
// with `nohash=1`, no state hash (hashing every submitted byte costs CPU time a timing run
// shouldn't count; `hash.txt` then says `none`);
// with `input=script.json`, a script of input events (runtime/abi `input`), each queued before
// the frame it's for; with `latency=n`, the main thread sends n pointer events through the DOM at
// random times while the frames run in real time, and saves when each reached the program
// (`latency.json`); with `keylatency=n`, n presses of the right arrow key instead, at least
// 250 ms apart; with `clip=n` (and `clipfrom=i`), the canvas recorded from frame i (0 unless
// given) for n frames, as a video (`clip.webm`: the main thread's MediaRecorder; paced frames
// play at their speed). With `features=none` (or a comma list of the ones to keep), the program
// sees only those optional device features (`std::gpu::features()`), though the device has them
// all: its fallback paths, measured on a device that has the features. With `checks=0`, the
// command checker is off, as in a release build's normal run, where WebGPU's own validation
// (an `uncapturederror`) is what stops a wrong command: a profile's runs.
//
// A program with a ticker (`std::tick::start`) runs it on the lockstep schedule (#43 §2.3):
// before frame i, the ticks up to ⌊(i + 1)·hz/fps⌋ run, and the frame waits for them, as on
// the native host. With `paced=1`, ticks run on the ticker's own clock instead and neither waits
// for the other, as in normal play: for time budgets. Either way, `ticks.json` has each tick's
// CPU time, and, unless `nohash=1` (or with `ticklog=1` beside it: the tick log without the
// frames' hash, for a paced run that's also a replay), `ticks.log` is the tick log (runtime/abi
// `ticks`: each tick's records and state hash), which `wrela-host --replay` replays. With `tickdelay=ms`, each tick is
// held that much longer; with `framedelay=ms`, each frame. With `saturate=1`, each frame starts
// as soon as the last is recorded, not at its time, with at most two frames on the GPU and the
// canvas left alone (it would pace them by the display): a GPU that sets its clock by its load
// stays busy, so a frame's GPU time is its work (GPU budgets). With `inflight=n` (paced), each
// frame still begins at its time, but up to n frames' GPU work may be in flight, as a browser
// lets a page's (without it, each waits for the last to finish on the GPU, so a paced frame's
// interval is its CPU time and its GPU time). With `snap=n` (and `snapfrom=i`), the screen every
// n frames from frame i (0 unless given), as `snap-<frame>.png`. With `salt=n`, each pipeline's entry points
// store n first, so no cache serves its pipelines (worker.ts `salt`), and `pipelines.json` has how
// long creating them all, at once, took (#42 AC4's cold pipelines). `load.json` has when the page opened
// and each file it loaded; `memory.json` the bytes of the program's GPU buffers and textures (at
// the end, and at most) and of its WASM memory (reserved, and grown to). It's part of the shipped bundle, so the agreement test runs
// the exact bytes a game ships, but only a page served from this machine (the test servers all
// bind 127.0.0.1) enters it: a game's public URL ignores `#test`.

import type { Loaded } from "./messages.ts";
import { pathProblem, pathUrl } from "./path.ts";

export interface TestParams {
  frames: number;
  width: number;
  height: number;
  fps: number;
  workers: number;
  /** Quanta of the voice to render (0: none). */
  audio: number;
  /** Seconds to play the voice in real time instead (0: none). */
  live: number;
  /** 1: time each pass on the GPU. */
  timestamps: number;
  /** 1: keep no state hash. */
  nohash: number;
  /** 1: keep the tick log (its ticks' state hashes) even with `nohash`. */
  ticklog: number;
  /** Paced, how many frames' GPU work may be in flight (0: each frame waits for the last's). */
  inflight: number;
  /** The screen saved every `snap` frames from `snapfrom` (0: never). */
  snap: number;
  snapfrom: number;
  /** A script of input events, relative to the page; "" for none. */
  input: string;
  /** The optional features the program may see: null for all the device has, else `none` or a
   * comma list. */
  features: string | null;
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
  /** Frames of the canvas to record as a video (0: none), and the frame it starts at. */
  clip: number;
  clipfrom: number;
  /** 0: the checker off, as a release build plays (a profile's runs); 1, the default: on, so a
   * test names the program's mistake. */
  checks: number;
  /** 1: at the end, draw the run's summary over the canvas and leave it there, for a device
   * that can't put its results (a phone on the LAN). */
  show: number;
  /** 1: the program may show the screen another way (`std::gpu::screen`): HDR, or Display P3.
   * Otherwise test mode keeps it `SCREEN_FORMAT`, so goldens and hashes don't change; with it the
   * run writes `screen.json` (the canvas's configuration, the screen's middle texel). */
  hdr: number;
}

export const TEST_DEFAULTS: TestParams = {
  frames: 60,
  width: 1920,
  height: 1080,
  fps: 60,
  workers: 1,
  audio: 0,
  live: 0,
  timestamps: 0,
  nohash: 0,
  ticklog: 0,
  inflight: 0,
  snap: 0,
  snapfrom: 0,
  input: "",
  features: null,
  latency: 0,
  keylatency: 0,
  paced: 0,
  tickdelay: 0,
  framedelay: 0,
  salt: 0,
  saturate: 0,
  clip: 0,
  clipfrom: 0,
  checks: 1,
  show: 0,
  hdr: 0,
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

/** Whether test mode may run on a page from `hostname`: this machine, or an address on a private
 * network (10/8, 172.16/12, 192.168/16, as `wrela run --lan` serves a device there), never a
 * name or a public address, so a shipped page doesn't enter it. */
export function mayTest(hostname: string): boolean {
  if (isLoopback(hostname)) return true;
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(hostname);
  if (m === null || m.slice(1).some((x) => Number(x) > 255)) return false;
  const [a, b] = [Number(m[1]), Number(m[2])];
  // As a LAN server's own addresses are (runtime/native `interfaces::private`): RFC 1918's, the
  // shared 100.64/10 (an overlay's, Tailscale's), and link-local.
  return a === 10 || (a === 172 && b >= 16 && b < 32) || (a === 192 && b === 168) || (a === 100 && b >= 64 && b < 128) || (a === 169 && b === 254);
}

/** The nearest-rank `p`th percentile of `xs`. */
function percentile(xs: number[], p: number): number {
  const sorted = [...xs].sort((a, b) => a - b);
  return sorted[Math.max(Math.ceil((sorted.length * p) / 100), 1) - 1] ?? 0;
}

/** A run's summary, as `show=1` draws it: the frames' intervals (their 50th, 95th and 99th
 * percentiles, and how many were over 33 ms), when the page was first playable (its program's
 * first line that says `playable`) and its first frame, how long its pipelines took, and the
 * adapter. */
export function summaryLines(
  run: { began_ms: number[]; printed: string[]; printed_in: number[]; opened_ms: number; pipelines_ms: number | null },
  adapter: string,
): string[] {
  const intervals = run.began_ms.slice(1).map((t, i) => t - run.began_ms[i]!);
  const ms = (x: number) => `${x.toFixed(1)} ms`;
  const lines = [
    `${intervals.length + 1} frames; intervals p50 ${ms(percentile(intervals, 50))}, p95 ${ms(percentile(intervals, 95))}, p99 ${ms(percentile(intervals, 99))}; over 33 ms: ${intervals.filter((x) => x > 33).length}`,
  ];
  const first = run.began_ms[0];
  if (first !== undefined) lines.push(`first frame ${ms(first - run.opened_ms)} after the page opened`);
  const said = run.printed.findIndex((l) => l.includes("playable"));
  const at = said < 0 ? undefined : run.began_ms[run.printed_in[said]!];
  lines.push(at === undefined ? "playable: not said" : `playable ${ms(at - run.opened_ms)}`);
  if (run.pipelines_ms !== null) lines.push(`pipelines ${ms(run.pipelines_ms)}`);
  lines.push(adapter || "an unnamed adapter");
  return lines;
}

/** Whether a URL fragment asks for test mode, whether or not its parameters are valid. */
export const asksForTest = (hash: string) => hash.replace(/^#/, "").split("&")[0] === "test";

/** Test mode's parameters from a URL fragment, or null if the fragment doesn't ask for it. */
export function parseTestParams(hash: string): TestParams | null {
  if (!asksForTest(hash)) return null;
  const [, ...pairs] = hash.replace(/^#/, "").split("&");
  const params = { ...TEST_DEFAULTS };
  for (const pair of pairs) {
    const separator = pair.indexOf("=");
    const key = separator < 0 ? pair : pair.slice(0, separator);
    const value = separator < 0 ? "" : pair.slice(separator + 1);
    if (key === "input") {
      if (pathProblem(value) !== undefined) {
        throw new Error(`test parameter input=${value} must be a path relative to the page`);
      }
      params.input = value;
      continue;
    }
    if (key === "checks") {
      if (value !== "0" && value !== "1") throw new Error(`test parameter checks=${value} must be 0 or 1`);
      params.checks = Number(value);
      continue;
    }
    if (key === "features") {
      if (value !== "none" && !/^[a-z0-9-]+(,[a-z0-9-]+)*$/.test(value)) {
        throw new Error(`test parameter features=${value} must be none or a comma list of features`);
      }
      params.features = value;
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

/** A script's literal path under the page, including percent signs that look like URL escapes. */
export const inputUrl = pathUrl;

/** The time of frame `i`, as both hosts compute it (runtime/abi `ticks`' `frame_time`, which the
 * vectors check): `i / fps` (the WASM call rounds it to f32). */
export const frameTime = (i: number, fps: number) => i / fps;

/** PUTs a file into the page's `results/` directory (a tool's server, `std::tool::serve`, or `wrela run`'s). */
export async function putResult(base: string, name: string, body: BodyInit): Promise<void> {
  const url = new URL(`results/${name}`, base);
  const response = await fetch(url, { method: "PUT", body, redirect: "error" });
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
