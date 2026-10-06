// The ticker's thread (#43 §2.1), the sim worker: the render worker starts it when the program
// starts its ticker (`std::tick::start`, the import `wrela.tick`). It is one more instance of
// the module on the shared memory, and it calls `__tick(thread, task, context, k)` once for
// each tick k:
// - on its own clock: tick k is due at t₀ + k/hz of visible time (frame.ts). Behind, it runs at
//   most CATCH_UP ticks a wake, then drops the rest of the debt and moves t₀ (std::tick's
//   `origin`). The tick number is the sim's clock, so a drop changes pacing, never results.
// - or, in test mode's lockstep schedule (#43 §2.3), up to the count the render worker sets
//   before each frame, which waits for them.
// At the start of each tick it stamps the input events that have arrived (the input ring's
// ticker reader, and a test script's) into the tick's records (#43 §2.2). The render worker
// talks to it through words in shared memory (`TickerControl`): a thread blocked in
// `Atomics.wait` can't receive messages.

import {
  EVENT_SIZE,
  EXPORT_TICK,
  MAX_TICK_RECORDS,
  THREAD_TICK,
  TICK_HASH,
  TICK_ORIGIN,
  TICK_RECORDS,
  TICK_WANT_HASH,
} from "./abi.gen.ts";
import { errorMessage } from "./errors.ts";
import { VisibleClock } from "./frame.ts";
import { eventsAt, InputRing, recordsAt } from "./input.ts";
import type { TickerStart } from "./messages.ts";
import { Lines } from "./lines.ts";
import { describeTrap, otherInstance, takePanicMessage, TrapError } from "./program.ts";
import { TickLogWriter } from "./ticks.ts";

/** Ticks a wake runs at most, behind; the rest of the debt is dropped (#43 §2.1). */
export const CATCH_UP = 4;

/** The ticker's schedules (`TickerControl.mode`). */
export const TickerMode = {
  /** Not yet: the render worker hasn't said how. */
  WAIT: 0,
  /** On the ticker's own clock: normal play, and test mode's paced schedule. */
  CLOCK: 1,
  /** Test mode's lockstep schedule: up to the render worker's target. */
  LOCKSTEP: 2,
} as const;

// The shared words, as Int32 indices; then, at byte 32, a float64: the visible time (ms) when
// the program started, which frames and ticks count from.
/** Bumped and notified whenever the render worker changes another word. */
const WAKE = 0;
const MODE = 1;
/** 1: stop, and report the ticks run (test mode). */
const STOP = 2;
/** Lockstep: run ticks until this many have run. */
const TARGET = 3;
/** Frames started: a script's events of those frames are the ticker's from its next tick. */
const FRAMES = 4;
/** Ticks run; notified after each. */
const DONE = 5;
/** Ticks the clock dropped, catching up. */
const DROPPED = 6;
/** 1: a tick trapped (DONE is notified too). */
const FAILED = 7;
const WORDS = 8;
const START = 32;

/** The words the render worker and the ticker's thread share. */
export class TickerControl {
  readonly #words: Int32Array;
  readonly #start: Float64Array;

  constructor(readonly buffer: SharedArrayBuffer) {
    this.#words = new Int32Array(buffer, 0, WORDS);
    this.#start = new Float64Array(buffer, START, 1);
  }

  static create(): TickerControl {
    return new TickerControl(new SharedArrayBuffer(START + 8));
  }

  #set(word: number, value: number): void {
    Atomics.store(this.#words, word, value);
    Atomics.add(this.#words, WAKE, 1);
    Atomics.notify(this.#words, WAKE);
  }

  /** The visible time (ms, `VisibleClock.at`) frames and ticks count from. */
  get start(): number {
    return this.#start[0]!;
  }

  set start(ms: number) {
    this.#start[0] = ms;
  }

  /** Starts the ticks on a schedule (`TickerMode`). */
  go(mode: number): void {
    this.#set(MODE, mode);
  }

  get mode(): number {
    return Atomics.load(this.#words, MODE);
  }

  /** Frame `frames - 1` starts: a script's events of it are the ticker's from its next tick. */
  framesStarted(frames: number): void {
    Atomics.store(this.#words, FRAMES, frames);
  }

  get frames(): number {
    return Atomics.load(this.#words, FRAMES);
  }

  /** Lockstep: run ticks until `target` have run. */
  runTo(target: number): void {
    this.#set(TARGET, target);
  }

  get target(): number {
    return Atomics.load(this.#words, TARGET);
  }

  /** Stops the ticks; in test mode the ticker's thread then reports them. */
  stop(clock?: VisibleClock): void {
    this.#set(STOP, 1);
    clock?.wake();
  }

  get stopped(): boolean {
    return Atomics.load(this.#words, STOP) !== 0;
  }

  get done(): number {
    return Atomics.load(this.#words, DONE);
  }

  get dropped(): number {
    return Atomics.load(this.#words, DROPPED);
  }

  get failed(): boolean {
    return Atomics.load(this.#words, FAILED) !== 0;
  }

  /** On the ticker's thread: tick `n - 1` has run. */
  ran(n: number): void {
    Atomics.store(this.#words, DONE, n);
    Atomics.notify(this.#words, DONE);
  }

  /** On the ticker's thread: `n` more ticks were dropped. */
  drop(n: number): void {
    Atomics.add(this.#words, DROPPED, n);
  }

  /** On the ticker's thread: a tick trapped. */
  fail(): void {
    Atomics.store(this.#words, FAILED, 1);
    Atomics.notify(this.#words, DONE);
  }

  /** On the ticker's thread: the wake count now, to wait on (`sleep`). */
  get wakes(): number {
    return Atomics.load(this.#words, WAKE);
  }

  /** On the ticker's thread: waits until the render worker changes a word after `seen`, or for
   * `ms` (forever if not given). */
  sleep(seen: number, ms?: number): void {
    Atomics.wait(this.#words, WAKE, seen, ms);
  }

  /** On the render worker, without blocking it: resolves when `n` ticks have run, or a tick
   * trapped. */
  async ranTo(n: number): Promise<void> {
    for (;;) {
      const done = Atomics.load(this.#words, DONE);
      if (done >= n || this.failed) return;
      const { async, value } = Atomics.waitAsync(this.#words, DONE, done);
      if (async) await value;
    }
  }
}

/** The ticks before frame `i` on the lockstep schedule: ⌊(i + 1)·hz/fps⌋ (as the native host
 * computes it, `wrela_host::lockstep_ticks`). */
export const lockstepTicks = (i: number, hz: number, fps: number) => Math.floor(((i + 1) * hz) / fps);

/** What the ticks did, for test mode's results (`ticks.json`). */
export interface TickReport {
  /** When each tick began (ms since 1970) and how long it took (ms). */
  began_ms: number[];
  cpu_ms: number[];
  /** Each tick's records, and state hash (16 hex digits) when hashes were kept. */
  records: number[];
  hashes: string[];
  /** Ticks dropped, catching up. */
  dropped: number;
}

/** What the ticker's thread tells the render worker. */
export type FromTicker =
  | { type: "fatal"; message: string }
  /** Stopped (test mode): the ticks, and the tick log when hashes were kept. */
  | { type: "ticks"; report: TickReport; log: Uint8Array<ArrayBuffer> | null };

type TickFn = (thread: number, task: number, context: number, tick: number) => void;

/** Runs the ticker until it's stopped, or a tick traps; `post` tells the render worker. */
export async function runTicker(start: TickerStart, post: (msg: FromTicker) => void): Promise<void> {
  const control = new TickerControl(start.control);
  let tick: TickFn;
  let lines: Lines | null = null;
  try {
    const instance = await otherInstance(start.module, start.memory, "the ticker");
    tick = instance.exports[EXPORT_TICK] as TickFn;
    lines = Lines.of(start.module);
  } catch (e) {
    control.fail();
    post({ type: "fatal", message: `the ticker couldn't start: ${errorMessage(e)}` });
    return;
  }
  const ticker = new Ticker(start, control, tick, lines);
  try {
    ticker.loop();
  } catch (e) {
    control.fail();
    post({ type: "fatal", message: errorMessage(e) });
    return;
  }
  post({ type: "ticks", report: ticker.report, log: ticker.log?.encode() ?? null });
}

const now = () => performance.timeOrigin + performance.now();

class Ticker {
  readonly clock: VisibleClock;
  readonly ring: InputRing;
  /** Events not yet stamped into a tick, oldest first. */
  readonly queue: Uint8Array[] = [];
  /** Frames whose script events are queued. */
  framesQueued = 0;
  next = 0;
  /** The clock's origin: tick k is due at t₀ + k/hz (seconds from `control.start`). */
  t0: number | null = null;
  readonly log: TickLogWriter | null;
  readonly report: TickReport = { began_ms: [], cpu_ms: [], records: [], hashes: [], dropped: 0 };
  /** Something to sleep on, for a test's delay. */
  readonly #nap = new Int32Array(new SharedArrayBuffer(4));

  constructor(
    readonly start: TickerStart,
    readonly control: TickerControl,
    readonly tick: TickFn,
    readonly lines: Lines | null,
  ) {
    this.clock = new VisibleClock(start.clock);
    this.ring = new InputRing(start.input);
    const view = new DataView(start.memory.buffer);
    // The first world's hash, which `start` reported before the ticker began.
    const first = view.getBigUint64(TICK_HASH, true);
    this.log = start.hashes ? new TickLogWriter(start.wasmHash, start.hz, first) : null;
  }

  /** Seconds of visible time since the program started. */
  seconds(): number {
    return (this.clock.at(now()) - this.control.start) / 1000;
  }

  loop(): void {
    const { control, start } = this;
    for (;;) {
      const seen = control.wakes;
      if (control.stopped) return;
      const mode = control.mode;
      if (mode === TickerMode.LOCKSTEP) {
        if (this.next < control.target) this.runTick();
        else control.sleep(seen);
        continue;
      }
      if (mode !== TickerMode.CLOCK) {
        control.sleep(seen);
        continue;
      }
      if (!this.clock.visible) {
        // Ticks pause while the page is hidden: visible time stands still, so they resume
        // where they were. (`stop` wakes this wait too.)
        this.clock.waitVisible(1000);
        continue;
      }
      const s = this.seconds();
      if (this.t0 === null) this.setOrigin(s);
      const t0 = this.t0!;
      // Ticks 0 to due - 1 are due by now.
      const due = Math.floor((s - t0) * start.hz) + 1;
      if (this.next < due) {
        const n = Math.min(due - this.next, CATCH_UP);
        for (let i = 0; i < n && !control.stopped; i++) this.runTick();
        const behind = due - this.next;
        if (behind > 0 && !control.stopped) {
          control.drop(behind);
          this.report.dropped += behind;
          this.setOrigin(t0 + behind / start.hz);
        }
        continue;
      }
      control.sleep(seen, Math.max((t0 + this.next / start.hz - s) * 1000, 0));
    }
  }

  /** Moves the clock's origin, which the program reads (`std::tick::origin`). */
  setOrigin(t0: number): void {
    this.t0 = t0;
    new DataView(this.start.memory.buffer).setFloat64(TICK_ORIGIN, t0, true);
  }

  /** Runs the next tick with the events that have arrived, at most MAX_TICK_RECORDS (the rest
   * wait for the next tick). */
  runTick(): void {
    const { start, control, queue } = this;
    const k = this.next;
    for (const frames = control.frames; this.framesQueued < frames; this.framesQueued++) {
      queue.push(...eventsAt(start.script, this.framesQueued));
    }
    if (queue.length < MAX_TICK_RECORDS) {
      const { events } = this.ring.readTicker(MAX_TICK_RECORDS - queue.length);
      for (let at = 0; at < events.length; at += EVENT_SIZE) queue.push(events.subarray(at, at + EVENT_SIZE));
    }
    queue.push(...recordsAt(start.script, k));
    const records = queue.splice(0, MAX_TICK_RECORDS);
    const bytes = new Uint8Array(records.length * EVENT_SIZE);
    records.forEach((r, i) => bytes.set(r, i * EVENT_SIZE));
    const memory = start.memory.buffer;
    const view = new DataView(memory);
    view.setUint32(TICK_RECORDS, records.length, true);
    new Uint8Array(memory).set(bytes, TICK_RECORDS + 4);
    view.setUint32(TICK_WANT_HASH, start.hashes ? 1 : 0, true);
    const began = now();
    try {
      this.tick(THREAD_TICK, start.task, start.context, k);
    } catch (e) {
      throw this.trapped(k, e);
    }
    const cpu = now() - began;
    if (start.delay > 0) Atomics.wait(this.#nap, 0, 0, start.delay);
    if (start.timing) {
      this.report.began_ms.push(began);
      this.report.cpu_ms.push(cpu);
      this.report.records.push(records.length);
    }
    if (this.log) {
      const hash = view.getBigUint64(TICK_HASH, true);
      this.log.push(bytes, hash);
      this.report.hashes.push(hash.toString(16).padStart(16, "0"));
    }
    this.next = k + 1;
    control.ran(this.next);
  }

  /** Tick `k` trapped: the program's panic, as the native host reports it. */
  trapped(k: number, e: unknown): TrapError {
    const msg = takePanicMessage(this.start.memory, THREAD_TICK);
    const what = describeTrap(e, this.lines);
    return new TrapError(`in tick ${k}: ${msg === null ? what : `panic: ${msg}: ${what}`}`);
  }
}
