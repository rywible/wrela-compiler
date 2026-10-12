// Input events (src/input.ts): their encoding, and the ring the main thread writes them into.

import { expect, test } from "bun:test";
import { EVENT_SIZE, EventKind } from "../src/abi.gen.ts";
import { InputRing, keyCode, keyEvent, padAxisEvent, padButtonEvent, parseScript, pointerEvent, textEvent } from "../src/input.ts";
import { listen, type PadState, padChanges, pollPads } from "../src/listen.ts";

test("the ring gives what was written, once, in order, with when", () => {
  const ring = InputRing.create();
  ring.write(textEvent(97), 1000.5);
  ring.write(textEvent(98), 1001.25);
  const first = ring.read();
  expect(first.events.length).toBe(2 * EVENT_SIZE);
  expect(first.events[8]).toBe(97);
  expect(first.events[EVENT_SIZE + 8]).toBe(98);
  expect(first.times).toEqual([1000.5, 1001.25]);
  expect(ring.read().events.length).toBe(0);
  // Another view of the same memory (the worker's) reads what this one writes.
  const worker = new InputRing(ring.buffer);
  ring.write(pointerEvent(1, 10, 20, 0, 0), 5);
  expect(worker.read().events.length).toBe(EVENT_SIZE);
});

test("a full ring drops what's written until it's read", () => {
  const ring = InputRing.create();
  for (let i = 0; i < InputRing.CAPACITY; i++) expect(ring.write(textEvent(i), i)).toBe(true);
  expect(ring.write(textEvent(1), 0)).toBe(false);
  expect(ring.read().events.length).toBe(InputRing.CAPACITY * EVENT_SIZE);
  expect(ring.write(textEvent(1), 0)).toBe(true);
});

test("the ring's unsigned counters cross the signed boundary", () => {
  const ring = InputRing.create();
  const counts = new Int32Array(ring.buffer, 0, 4);
  Atomics.store(counts, 0, 0x7fff_ffff);
  Atomics.store(counts, 1, 0x7fff_ffff);
  expect(ring.write(textEvent(1), 1)).toBe(true);
  expect(ring.write(textEvent(2), 2)).toBe(true);
  expect(ring.write(textEvent(3), 3)).toBe(true);
  const read = ring.read();
  expect(read.events.length).toBe(3 * EVENT_SIZE);
  expect([read.events[8], read.events[EVENT_SIZE + 8], read.events[2 * EVENT_SIZE + 8]]).toEqual([1, 2, 3]);
  expect(read.times).toEqual([1, 2, 3]);
});

test("detaching a replaced ticker lets frames release ring space", () => {
  const ring = InputRing.create();
  ring.attachTicker();
  for (let i = 0; i < InputRing.CAPACITY; i++) expect(ring.write(textEvent(i), i)).toBe(true);
  ring.read();
  expect(ring.write(textEvent(1), 0)).toBe(false);
  ring.detachTicker();
  expect(ring.write(textEvent(1), 0)).toBe(true);
});

test("detaching a ticker skips events already overwritten while frames fell behind", () => {
  const ring = InputRing.create();
  ring.attachTicker();
  const ticker = new Uint8Array(EVENT_SIZE);
  for (let i = 0; i < InputRing.CAPACITY + 5; i++) {
    expect(ring.write(textEvent(i), i)).toBe(true);
    expect(ring.readTicker(ticker, 0, 1)).toBe(1);
  }
  ring.detachTicker();
  const { events, times } = ring.read();
  expect(events.length).toBe(InputRing.CAPACITY * EVENT_SIZE);
  expect(times).toEqual(Array.from({ length: InputRing.CAPACITY }, (_, i) => i + 5));
  expect(ring.read().events.length).toBe(0);
  expect(ring.write(textEvent(1), 0)).toBe(true);
});

test("text is characters: a lone surrogate is refused, as the native host refuses it", () => {
  expect(parseScript('[{"frame": 0, "type": "text", "text": "a\\ud83d\\ude00"}]').length).toBe(2);
  expect(() => parseScript('[{"frame": 0, "type": "text", "text": "\\ud800"}]')).toThrow("lone surrogate");
});

test("keys are the ABI's numbers", () => {
  expect(keyCode("KeyA")).toBe(1);
  expect(keyCode("Tab")).toBe(40);
  expect(keyCode("IntlRo")).toBe(0);
});

test("losing focus or a pointer releases controls whose up event cannot arrive", () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  const page = new EventTarget();
  Object.defineProperty(globalThis, "window", { value: page, configurable: true });
  try {
    const canvas = Object.assign(new EventTarget(), {
      getBoundingClientRect: () => ({ left: 0, top: 0, width: 100, height: 100 }),
      setPointerCapture() {},
    });
    const ring = InputRing.create();
    listen(canvas as unknown as HTMLCanvasElement, ring, () => 1, () => ({ width: 100, height: 100 }));
    const event = (type: string, fields: Record<string, unknown>) => Object.assign(new Event(type), {
      shiftKey: false, ctrlKey: false, altKey: false, metaKey: false, ...fields,
    });
    page.dispatchEvent(event("keydown", { code: "KeyA", key: "a", repeat: false }));
    canvas.dispatchEvent(event("pointerdown", { clientX: 5, clientY: 6, pointerId: 1, button: 0 }));
    ring.read();
    page.dispatchEvent(new Event("blur"));
    expect(ring.read().events).toEqual(new Uint8Array([
      ...keyEvent(false, keyCode("KeyA"), false, 0), ...pointerEvent(EventKind.PointerUp, 5, 6, 0, 0),
    ]));
    page.dispatchEvent(new Event("blur"));
    expect(ring.read().events.length).toBe(0);
    canvas.dispatchEvent(event("pointerdown", { clientX: 8, clientY: 9, pointerId: 2, button: 2 }));
    ring.read();
    canvas.dispatchEvent(event("pointercancel", { pointerId: 2 }));
    expect(ring.read().events).toEqual(pointerEvent(EventKind.PointerUp, 8, 9, 2, 0));
    canvas.dispatchEvent(event("lostpointercapture", { pointerId: 2 }));
    expect(ring.read().events.length).toBe(0);
  } finally {
    if (original) Object.defineProperty(globalThis, "window", original);
    else delete (globalThis as Record<string, unknown>)["window"];
  }
});

test("a pad sends what changed: a button, a stick past its noise, and a let-go when it goes", () => {
  const last: (PadState | null)[] = [null, null, null, null];
  const button = (value: number, pressed: boolean) => ({ value, pressed });
  const pad = (buttons: { value: number; pressed: boolean }[], axes: number[]) => ({
    connected: true,
    mapping: "standard",
    buttons,
    axes,
  });
  // Pad 1 arrives with East pressed and its left stick to the right.
  const first = padChanges([null, pad([button(0, false), button(1, true)], [0.5, 0])], last);
  expect(first).toEqual([padButtonEvent(1, 1, 1, true), padAxisEvent(1, 0, 0.5)]);
  // Noise under a step of 1/1024 sends nothing; a trigger half down does.
  const second = padChanges([null, pad([button(0.5, false), button(1, true)], [0.5002, 0])], last);
  expect(second).toEqual([padButtonEvent(1, 0, 0.5, false)]);
  // The pad goes: what it held is let go.
  const gone = padChanges([null, null], last);
  expect(gone).toEqual([padButtonEvent(1, 0, 0, false), padButtonEvent(1, 1, 0, false), padAxisEvent(1, 0, 0)]);
  expect(last[1]).toBe(null);
});

test("pad polling delivers held controls changed while the parent was inactive", () => {
  const gamepads = Object.getOwnPropertyDescriptor(navigator, "getGamepads");
  const animation = Object.getOwnPropertyDescriptor(globalThis, "requestAnimationFrame");
  const pending: FrameRequestCallback[] = [];
  let active = true;
  let reading = { connected: true, mapping: "standard", buttons: [{ value: 1, pressed: true }], axes: [0.5] };
  Object.defineProperty(navigator, "getGamepads", { value: () => [reading], configurable: true });
  Object.defineProperty(globalThis, "requestAnimationFrame", { value: (callback: FrameRequestCallback) => pending.push(callback), configurable: true });
  try {
    const ring = InputRing.create();
    const poll = () => pending.shift()!(0);
    pollPads(ring, () => 1, () => active);
    poll();
    expect(ring.read().events).toEqual(new Uint8Array([...padButtonEvent(0, 0, 1, true), ...padAxisEvent(0, 0, 0.5)]));
    active = false;
    reading = { ...reading, buttons: [{ value: 0, pressed: false }], axes: [0] };
    poll();
    expect(ring.read().events.length).toBe(0);
    active = true;
    poll();
    expect(ring.read().events).toEqual(new Uint8Array([...padButtonEvent(0, 0, 0, false), ...padAxisEvent(0, 0, 0)]));
    active = false;
    reading = { ...reading, buttons: [{ value: 1, pressed: true }], axes: [-0.5] };
    poll();
    expect(ring.read().events.length).toBe(0);
    active = true;
    poll();
    expect(ring.read().events).toEqual(new Uint8Array([...padButtonEvent(0, 0, 1, true), ...padAxisEvent(0, 0, -0.5)]));
    poll();
    expect(ring.read().events.length).toBe(0);
  } finally {
    if (gamepads) Object.defineProperty(navigator, "getGamepads", gamepads);
    else Reflect.deleteProperty(navigator, "getGamepads");
    if (animation) Object.defineProperty(globalThis, "requestAnimationFrame", animation);
    else Reflect.deleteProperty(globalThis, "requestAnimationFrame");
  }
});

test("a pad not in the standard mapping sends nothing", () => {
  const last: (PadState | null)[] = [null, null, null, null];
  const raw = { connected: true, mapping: "", buttons: [{ value: 1, pressed: true }], axes: [0.5] };
  expect(padChanges([raw], last)).toEqual([]);
  expect(last[0]).toBe(null);
});

test("a disconnected pad releases only controls that it held", () => {
  const last: (PadState | null)[] = [null, null, null, null];
  const pad = {
    connected: true,
    mapping: "standard",
    buttons: [{ value: 0, pressed: false }, { value: 1, pressed: true }],
    axes: [0, 0, 0, 0.5],
  };
  expect(padChanges([pad], last)).toEqual([padButtonEvent(0, 1, 1, true), padAxisEvent(0, 3, 0.5)]);
  expect(padChanges([], last)).toEqual([padButtonEvent(0, 1, 0, false), padAxisEvent(0, 3, 0)]);
});

test("a standard pad sends only ABI controls and releases controls missing from a later reading", () => {
  const last: (PadState | null)[] = [null, null, null, null];
  const button = (value: number) => ({ value, pressed: value > 0 });
  const full = {
    connected: true,
    mapping: "standard",
    buttons: Array.from({ length: 18 }, (_, i) => button(i === 16 || i === 17 ? 1 : 0)),
    axes: [0, 0, 0, 0.5, 1],
  };
  expect(padChanges([full], last)).toEqual([padButtonEvent(0, 16, 1, true), padAxisEvent(0, 3, 0.5)]);
  expect(padChanges([{ ...full, buttons: [], axes: [] }], last)).toEqual([
    padButtonEvent(0, 16, 0, false),
    padAxisEvent(0, 3, 0),
  ]);
});
