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

import { EVENT_SIZE, EXPORT_TICK, MAX_TICK_RECORDS, THREAD_TICK, TICK_HASH, TICK_ORIGIN, TICK_RECORDS } from "./abi.gen.ts";
import { errorMessage } from "./errors.ts";
import { VisibleClock } from "./frame.ts";
import { type Arrivals, arrivals, epochNow, InputRing } from "./input.ts";
import type { FromTicker, Replay, TickerStart, TickReport } from "./messages.ts";
import { Lines } from "./lines.ts";
import { otherInstance, TrapError, trapMessage } from "./program.ts";
import { holdFor } from "./testmode.ts";
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

// The shared words, as Int32 indices.
/** Bumped and notified whenever the render worker changes another word, or the page shows or
 * hides. */
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
/** 1: a tick trapped (DONE is notified too). */
const FAILED = 6;
const WORDS = 7;

/** The words the render worker and the ticker's thread share. */
export class TickerControl {
  readonly #words: Int32Array;

  constructor(readonly buffer: SharedArrayBuffer) {
    this.#words = new Int32Array(buffer, 0, WORDS);
  }

  static create(): TickerControl {
    return new TickerControl(new SharedArrayBuffer(WORDS * 4));
  }

  #set(word: number, value: number): void {
    Atomics.store(this.#words, word, value);
    this.wake();
  }

  /** Wakes the ticker's thread to look again: a word changed, or the page showed or hid. */
  wake(): void {
    Atomics.add(this.#words, WAKE, 1);
    Atomics.notify(this.#words, WAKE);
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
  stop(): void {
    this.#set(STOP, 1);
  }

  get stopped(): boolean {
    return Atomics.load(this.#words, STOP) !== 0;
  }

  get failed(): boolean {
    return Atomics.load(this.#words, FAILED) !== 0;
  }

  /** On the ticker's thread: tick `n - 1` has run. */
  ran(n: number): void {
    Atomics.store(this.#words, DONE, n);
    Atomics.notify(this.#words, DONE);
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

  /** On the ticker's thread: waits until something wakes it after `seen` (`wake`), or for `ms`
   * (forever if not given). */
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

/** The ticks before frame `i` on the lockstep schedule: ⌊(i + 1)·hz/fps⌋ (runtime/abi `ticks`'
 * `lockstep_ticks`, which the vectors check). */
export const lockstepTicks = (i: number, hz: number, fps: number) => Math.floor(((i + 1) * hz) / fps);

/** What a build hands the build that replaces it while it runs (hot reload): the ticks to run
 * again, each with its records, by tick (those it ran, and those it was still to replay and hadn't
 * reached), and how many ticks there were. As runtime/abi's `ticks::carried`; the vectors check
 * both. */
export function carried<R>(
  ran: readonly [number, R][],
  replay: { upto: number; ticks: readonly [number, R][] } | null,
  next: number,
): { upto: number; ticks: [number, R][] } {
  const ticks: [number, R][] = [...ran];
  // A set of the ticks run: a session's reloads each carry all its ticks so far.
  const run = new Set(ran.map(([t]) => t));
  for (const [k, r] of replay?.ticks ?? []) if (!run.has(k)) ticks.push([k, r]);
  ticks.sort((a, b) => a[0] - b[0]);
  return { upto: Math.max(next, replay?.upto ?? 0), ticks };
}

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
  post({ type: "ticks", report: ticker.report, log: ticker.log?.encode() ?? null, replay: ticker.replayed() });
}

/** Where a tick's records are, after their count. */
const RECORDS = TICK_RECORDS + 4;
/** A frame's or tick's script events, when the script has none. */
const NONE: readonly Uint8Array[] = [];

class Ticker {
  readonly clock: VisibleClock;
  readonly ring: InputRing;
  /** Test mode: a script's events, by frame and by tick. */
  readonly script: Arrivals;
  /** Events not yet stamped into a tick, oldest first. */
  readonly queue: Uint8Array[] = [];
  /** Frames whose script events are queued. */
  framesQueued = 0;
  next = 0;
  /** The clock's origin: tick k is due at t₀ + k/hz (seconds of visible time,
   * `VisibleClock.seconds`). */
  t0: number | null = null;
  readonly log: TickLogWriter | null;
  readonly report: TickReport = { cpu_ms: [] };
  /** The records of each tick run that had any: what a build that replaces this one replays.
   * Kept only on a live page (`TickerStart.replayable`). */
  readonly ran: [number, Uint8Array<ArrayBuffer>][] = [];
  /** Hot reload: the ticks the replaced build ran, run again first with their records. */
  readonly replay: { upto: number; ticks: Map<number, Uint8Array<ArrayBuffer>> } | null;
  /** Views of the shared memory, made once: a view of shared memory stays valid as it grows,
   * and the ticker's words and records are below the heap. */
  readonly #view: DataView;
  readonly #bytes: Uint8Array;

  constructor(
    readonly start: TickerStart,
    readonly control: TickerControl,
    readonly tick: TickFn,
    readonly lines: Lines | null,
  ) {
    this.clock = new VisibleClock(start.clock);
    this.ring = new InputRing(start.input);
    this.script = arrivals(start.script);
    this.#view = new DataView(start.memory.buffer);
    this.#bytes = new Uint8Array(start.memory.buffer);
    // The first world's hash, which `start` reported before the ticker began.
    const first = this.#view.getBigUint64(TICK_HASH, true);
    this.log = start.hashes ? new TickLogWriter(start.wasmHash, start.hz, first) : null;
    const r = start.replay;
    this.replay = r && r.upto > 0 ? { upto: r.upto, ticks: new Map(r.ticks) } : null;
    // A script's events of the frames started before the reload reached the replaced build.
    if (r) this.framesQueued = control.frames;
    // The replaced build's clock goes on: its ticks were due when they were.
    if (r?.origin != null) this.setOrigin(r.origin);
  }

  /** Whether the next tick is one the replaced build ran. */
  get replaying(): boolean {
    return this.replay !== null && this.next < this.replay.upto;
  }

  /** What a build that replaces this one replays: the ticks run (and those still to replay),
   * with their records, and the clock's origin. */
  replayed(): Replay {
    const replay = this.replay && { upto: this.replay.upto, ticks: [...this.replay.ticks] };
    return { ...carried(this.ran, replay, this.next), origin: this.t0 };
  }

  loop(): void {
    const { control, start } = this;
    for (;;) {
      const seen = control.wakes;
      if (control.stopped) return;
      const mode = control.mode;
      // Hot reload, on the ticker's own clock: the replaced build's ticks first, at once.
      if (mode === TickerMode.CLOCK && this.replaying) {
        this.runTick();
        continue;
      }
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
        // where they were. (The render worker wakes this sleep when the page shows.)
        control.sleep(seen, 1000);
        continue;
      }
      const s = this.clock.seconds(epochNow());
      if (this.t0 === null) this.setOrigin(s);
      const t0 = this.t0!;
      // Ticks 0 to due - 1 are due by now.
      const due = Math.floor((s - t0) * start.hz) + 1;
      if (this.next < due) {
        const n = Math.min(due - this.next, CATCH_UP);
        for (let i = 0; i < n && !control.stopped; i++) this.runTick();
        const behind = due - this.next;
        if (behind > 0 && !control.stopped) {
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
    this.#view.setFloat64(TICK_ORIGIN, t0, true);
  }

  /** Runs the next tick with the events that have arrived, at most MAX_TICK_RECORDS (the rest
   * wait for the next tick). They're written straight into its records, in order: what earlier
   * ticks left, a script's events of the frames started since, the input ring's, then a
   * script's records of this tick. */
  runTick(): void {
    const { start, control, queue } = this;
    const k = this.next;
    let n = 0;
    if (this.replaying) {
      // The records the replaced build's tick had; what arrives meanwhile waits.
      const logged = this.replay!.ticks.get(k);
      if (logged) {
        this.#bytes.set(logged, RECORDS);
        n = logged.length / EVENT_SIZE;
      }
    } else {
      n = Math.min(queue.length, MAX_TICK_RECORDS);
      for (let i = 0; i < n; i++) this.#bytes.set(queue[i]!, RECORDS + i * EVENT_SIZE);
      if (n > 0) queue.splice(0, n);
      for (const frames = control.frames; this.framesQueued < frames; this.framesQueued++) {
        n = this.#stamp(this.script.frames.get(this.framesQueued) ?? NONE, n);
      }
      if (n < MAX_TICK_RECORDS) n += this.ring.readTicker(this.#bytes, RECORDS + n * EVENT_SIZE, MAX_TICK_RECORDS - n);
      n = this.#stamp(this.script.ticks.get(k) ?? NONE, n);
    }
    this.#view.setUint32(TICK_RECORDS, n, true);
    // The records, taken before the tick runs, where they're kept: for the log, and for a build
    // that replaces this one.
    const replayed = start.replayable && n > 0;
    const records = this.log !== null || replayed ? this.#bytes.slice(RECORDS, RECORDS + n * EVENT_SIZE) : null;
    if (replayed) this.ran.push([k, records!]);
    const began = epochNow();
    try {
      this.tick(THREAD_TICK, start.task, start.context, k);
    } catch (e) {
      // The program's panic, as the native host reports it.
      throw new TrapError(`in tick ${k}: ${trapMessage(e, start.memory, THREAD_TICK, this.lines)}`);
    }
    const cpu = epochNow() - began;
    if (start.delay > 0) holdFor(start.delay);
    if (start.timing) this.report.cpu_ms.push(cpu);
    if (this.log) this.log.push(records!, this.#view.getBigUint64(TICK_HASH, true));
    this.next = k + 1;
    control.ran(this.next);
  }

  /** Stamps `events` into this tick's records after the first `n`, as many as fit; the rest are
   * queued for the next tick. Returns how many records the tick has now. */
  #stamp(events: readonly Uint8Array[], n: number): number {
    for (const e of events) {
      if (n < MAX_TICK_RECORDS) this.#bytes.set(e, RECORDS + n++ * EVENT_SIZE);
      else this.queue.push(e);
    }
    return n;
  }
}
