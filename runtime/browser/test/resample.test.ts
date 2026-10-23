// A held pointer's moves resampled to the frames (src/resample.ts).

import { expect, test } from "bun:test";
import { EVENT_SIZE, EventKind } from "../src/abi.gen.ts";
import { keyEvent, pointerEvent } from "../src/input.ts";
import { Resampler } from "../src/resample.ts";

const join = (events: Uint8Array[]) => new Uint8Array(events.flatMap((e) => [...e]));

/** Each event's kind and position. */
function read(bytes: Uint8Array): { kind: number; x: number; y: number }[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return Array.from({ length: bytes.length / EVENT_SIZE }, (_, i) => ({
    kind: view.getUint32(i * EVENT_SIZE, true),
    x: view.getFloat32(i * EVENT_SIZE + 8, true),
    y: view.getFloat32(i * EVENT_SIZE + 12, true),
  }));
}

test("empty frames reuse idle input and still advance a held pointer", () => {
  const r = new Resampler();
  const empty = new Uint8Array(0);
  expect(r.take(empty, [], 0)).toBe(empty);
  r.take(pointerEvent(EventKind.PointerDown, 0, 0, 0, 0), [0], 0);
  const first = read(r.take(pointerEvent(EventKind.PointerMove, 0, 10, 0, 0), [10], 10));
  expect(first).toHaveLength(1);
  expect(first[0]).toMatchObject({ kind: EventKind.PointerMove, x: 0 });
  expect(first[0]!.y).toBeGreaterThan(0);
  expect(first[0]!.y).toBeLessThan(10);
  expect(read(r.take(empty, [], 20))).toEqual([{ kind: EventKind.PointerMove, x: 0, y: 10 }]);
  r.take(pointerEvent(EventKind.PointerUp, 0, 10, 0, 0), [21], 21);
  expect(r.take(empty, [], 30)).toBe(empty);
});

test("a held finger moves evenly from frame to frame, however unevenly its moves arrive", () => {
  const r = new Resampler();
  const frame = 1000 / 60;
  // A finger moving 1 px a ms, sampled every 4 ms; frame n reads the samples that arrived by
  // then, some a frame late, in a fixed uneven pattern.
  r.take(pointerEvent(EventKind.PointerDown, 0, 0, 0, 0), [0], 0);
  const late = [false, true, false, false, true, true, false, false, true, false];
  const ys: number[] = [];
  let next = 4;
  for (let n = 1; n <= 60; n++) {
    const now = n * frame;
    const cut = late[n % late.length] ? now - frame : now;
    const events: Uint8Array[] = [];
    const times: number[] = [];
    while (next <= cut) {
      events.push(pointerEvent(EventKind.PointerMove, 0, next, 0, 0));
      times.push(next);
      next += 4;
    }
    const moves = read(r.take(join(events), times, now));
    expect(moves.length).toBeLessThanOrEqual(1);
    if (moves.length === 1) ys.push(moves[0]!.y);
  }
  // Once settled, each frame moves the finger's speed times a frame, give or take a tenth.
  const steps = ys.slice(20).map((y, i, a) => (i > 0 ? y - a[i - 1]! : frame)).slice(1);
  expect(steps.length).toBeGreaterThan(30);
  for (const s of steps) expect(Math.abs(s - frame)).toBeLessThan(frame * 0.1);
  // Behind the finger no more than its samples come late: a frame, and the gap between two.
  expect(60 * frame - ys[ys.length - 1]!).toBeLessThan(frame + 4 + 2);
});

test("a finger that rests is shown where it rests", () => {
  const r = new Resampler();
  r.take(pointerEvent(EventKind.PointerDown, 5, 5, 0, 0), [0], 0);
  let last = { kind: 0, x: 0, y: 0 };
  for (const e of read(r.take(pointerEvent(EventKind.PointerMove, 5, 40, 0, 0), [8], 16))) last = e;
  for (let n = 2; n < 20; n++) for (const e of read(r.take(new Uint8Array(0), [], n * 16))) last = e;
  expect(last).toEqual({ kind: EventKind.PointerMove, x: 5, y: 40 });
});

test("moves stay resampled while another pointer button remains held", () => {
  const r = new Resampler();
  r.take(join([
    pointerEvent(EventKind.PointerDown, 0, 0, 0, 0),
    pointerEvent(EventKind.PointerDown, 0, 0, 2, 0),
    pointerEvent(EventKind.PointerUp, 0, 0, 0, 0),
  ]), [0, 1, 2], 2);
  const moves = read(r.take(join([
    pointerEvent(EventKind.PointerMove, 0, 10, 0, 0),
    pointerEvent(EventKind.PointerMove, 0, 14, 0, 0),
  ]), [10, 14], 16));
  expect(moves).toHaveLength(1);
  expect(moves[0]!.kind).toBe(EventKind.PointerMove);
  expect(moves[0]!.y).toBeGreaterThan(0);
  expect(moves[0]!.y).toBeLessThan(10);
  r.take(pointerEvent(EventKind.PointerUp, 0, 14, 2, 0), [17], 17);
  const hover = join([
    pointerEvent(EventKind.PointerMove, 0, 20, 0, 0),
    pointerEvent(EventKind.PointerMove, 0, 24, 0, 0),
  ]);
  expect(r.take(hover, [20, 24], 24)).toEqual(hover);
});

test("a press, a release, a move with nothing held and other events pass as they come", () => {
  const r = new Resampler();
  const hover = pointerEvent(EventKind.PointerMove, 3, 4, 0, 0);
  const down = pointerEvent(EventKind.PointerDown, 3, 4, 0, 0);
  const key = keyEvent(true, 1, false, 0);
  expect(r.take(join([hover, down, key]), [0, 1, 2], 10)).toEqual(join([hover, down, key]));
  // A release comes after the frame's move, and a move after it isn't held.
  const out = read(r.take(join([pointerEvent(EventKind.PointerMove, 3, 30, 0, 0), pointerEvent(EventKind.PointerUp, 3, 30, 0, 0), pointerEvent(EventKind.PointerMove, 9, 9, 0, 0)]), [20, 21, 22], 30));
  expect(out.map((e) => e.kind)).toEqual([EventKind.PointerMove, EventKind.PointerUp, EventKind.PointerMove]);
  expect(out[2]).toEqual({ kind: EventKind.PointerMove, x: 9, y: 9 });
});
