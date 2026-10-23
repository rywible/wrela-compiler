// A held pointer's moves, resampled to the frames that show them. The main thread reads a
// pointer's moves when the browser hands them over, and the worker's frames keep their own clock:
// read as they come, a frame sometimes finds no move and the next finds two frames' worth, and
// what a finger drags stutters. So while a pointer is held, a frame at time `t` gets one move: where
// the pointer was at `t - behind`, between the samples around that moment. `behind` covers how far
// a frame's newest sample has trailed it over the last moments while the pointer moved, so a frame
// has the sample its moment needs: it grows at once when a frame's newest sample trails it farther
// than any of those, and shrinks slowly once the farthest is past. So the moment shown advances by the frames' own steps, however
// unevenly the samples arrive. A move with nothing held, a press, a release and
// every other event pass as they come, in order.

import { EVENT_SIZE, EventKind } from "./abi.gen.ts";
import { pointerEvent } from "./input.ts";

type Sample = { x: number; y: number; t: number };

/** What `behind` starts at, and how long how late a sample came counts (ms); how much of itself
 * `behind` sheds a frame while more than that. */
const START = 8;
const KEPT = 2000;
const SHED = 0.005;

export class Resampler {
  #buttons = 0;
  /** A held pointer's samples since the moment shown, oldest first: its press, then its moves. */
  #samples: Sample[] = [];
  #modifiers = 0;
  /** How far behind a frame's time the pointer is shown, in ms. */
  #behind = START;
  /** How far behind each frame of the last `KEPT` ms needed the pointer shown, oldest first. */
  #needs: { t: number; need: number }[] = [];
  /** The times of the samples come since the last frame; the last frame's time and its newest
   * sample's. */
  #fresh: number[] = [];
  #last: { frame: number; newest: number } | null = null;
  #shown: readonly [number, number] = [0, 0];

  /** The events a frame at `now` gets of `events`, those read since the last frame, written at
   * `times` (ms, on `now`'s clock). */
  take(events: Uint8Array, times: readonly number[], now: number): Uint8Array {
    if (events.length === 0 && this.#buttons === 0) return events;
    const view = new DataView(events.buffer, events.byteOffset, events.byteLength);
    const out: Uint8Array[] = [];
    let stepped = false;
    for (let i = 0; (i + 1) * EVENT_SIZE <= events.length; i++) {
      const off = i * EVENT_SIZE;
      const kind = view.getUint32(off, true);
      const x = view.getFloat32(off + 8, true);
      const y = view.getFloat32(off + 12, true);
      const t = times[i] ?? now;
      if (kind === EventKind.PointerMove && this.#buttons !== 0) {
        this.#modifiers = view.getUint32(off + 4, true);
        this.#samples.push({ x, y, t });
        this.#fresh.push(t);
        continue;
      }
      // A release comes after this frame's move, from where the pointer is shown.
      if (kind === EventKind.PointerUp && this.#buttons !== 0) {
        this.#step(now, out);
        stepped = true;
        this.#buttons &= ~(1 << view.getUint32(off + 16, true));
        if (this.#buttons === 0) this.#samples = [];
      }
      out.push(events.subarray(off, off + EVENT_SIZE));
      if (kind === EventKind.PointerDown) {
        this.#buttons |= 1 << view.getUint32(off + 16, true);
        this.#samples = [{ x, y, t }];
        this.#shown = [x, y];
        this.#fresh = [];
        this.#last = null;
      }
    }
    if (this.#buttons !== 0 && !stepped) this.#step(now, out);
    const bytes = new Uint8Array(out.length * EVENT_SIZE);
    out.forEach((e, i) => bytes.set(e, i * EVENT_SIZE));
    return bytes;
  }

  /** Adds the move to where the pointer was at `now - behind`, if it moved. */
  #step(now: number, out: Uint8Array[]): void {
    const samples = this.#samples;
    const newest = samples[samples.length - 1]!;
    // How far this frame's newest sample trails it, and the last frame's, if samples come since
    // were taken before it: the pointer was moving, and that frame lacked them.
    const needs = (trail: number) => this.#needs.push({ t: now, need: trail + 1 });
    if (this.#fresh.length > 0) {
      needs(now - newest.t);
      if (this.#last !== null && Math.min(...this.#fresh) <= this.#last.frame) needs(this.#last.frame - this.#last.newest);
      this.#fresh = [];
    }
    this.#last = { frame: now, newest: newest.t };
    while (this.#needs.length > 0 && this.#needs[0]!.t < now - KEPT) this.#needs.shift();
    const need = this.#needs.reduce((m, n) => Math.max(m, n.need), 0);
    this.#behind = need > this.#behind ? need : Math.max(need, this.#behind * (1 - SHED));
    const at = Math.min(now - this.#behind, newest.t);
    let j = samples.findIndex((s) => s.t >= at);
    if (j < 0) j = samples.length - 1;
    const b = samples[j]!;
    const a = samples[Math.max(j - 1, 0)]!;
    const k = b.t > a.t ? Math.min(Math.max((at - a.t) / (b.t - a.t), 0), 1) : 1;
    const x = a.x + (b.x - a.x) * k;
    const y = a.y + (b.y - a.y) * k;
    // The samples before the pair around the moment aren't needed again.
    samples.splice(0, Math.max(j - 1, 0));
    if (x === this.#shown[0] && y === this.#shown[1]) return;
    this.#shown = [x, y];
    out.push(pointerEvent(EventKind.PointerMove, x, y, 0, this.#modifiers));
  }
}
