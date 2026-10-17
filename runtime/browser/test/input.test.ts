// Input events (src/input.ts): their encoding, and the ring the main thread writes them into.

import { expect, test } from "bun:test";
import { EVENT_SIZE, EventKind } from "../src/abi.gen.ts";
import { composeEvents, InputRing, keyCode, keyEvent, padAxisEvent, padButtonEvent, parseScript, pasteEvent, pointerEvent, textEvent } from "../src/input.ts";
import { type Forward, listen, type PadState, padChanges, pollPads } from "../src/listen.ts";
import { domEvent, onPage } from "./page.ts";

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

test("losing focus or a pointer releases controls whose up event cannot arrive", () => onPage((page) => {
  const canvas = page.createElement("canvas");
  const ring = InputRing.create();
  listen(canvas as unknown as HTMLCanvasElement, ring, () => 1, () => ({ width: 100, height: 100 }));
  page.dispatchEvent(domEvent("keydown", { code: "KeyA", key: "a", repeat: false }));
  canvas.dispatchEvent(domEvent("pointerdown", { clientX: 5, clientY: 6, pointerId: 1, button: 0 }));
  ring.read();
  page.dispatchEvent(new Event("blur"));
  expect(ring.read().events).toEqual(new Uint8Array([
    ...keyEvent(false, keyCode("KeyA"), false, 0), ...pointerEvent(EventKind.PointerUp, 5, 6, 0, 0),
  ]));
  page.dispatchEvent(new Event("blur"));
  expect(ring.read().events.length).toBe(0);
  canvas.dispatchEvent(domEvent("pointerdown", { clientX: 8, clientY: 9, pointerId: 2, button: 2 }));
  ring.read();
  canvas.dispatchEvent(domEvent("pointercancel", { pointerId: 2 }));
  expect(ring.read().events).toEqual(pointerEvent(EventKind.PointerUp, 8, 9, 2, 0));
  canvas.dispatchEvent(domEvent("lostpointercapture", { pointerId: 2 }));
  expect(ring.read().events.length).toBe(0);
}));

test("a finger's move is where the browser predicts it, and a mouse's where it is", () => onPage((page) => {
  const canvas = page.createElement("canvas");
  const ring = InputRing.create();
  listen(canvas as unknown as HTMLCanvasElement, ring, () => 1, () => ({ width: 100, height: 100 }));
  const ahead = () => [{ clientX: 31, clientY: 42 }, { clientX: 33, clientY: 47 }];
  canvas.dispatchEvent(domEvent("pointermove", { clientX: 30, clientY: 40, pointerId: 1, pointerType: "touch", getPredictedEvents: ahead }));
  expect(ring.read().events).toEqual(pointerEvent(EventKind.PointerMove, 33, 47, 0, 0));
  // No prediction yet: where it is.
  canvas.dispatchEvent(domEvent("pointermove", { clientX: 30, clientY: 40, pointerId: 1, pointerType: "touch", getPredictedEvents: () => [] }));
  expect(ring.read().events).toEqual(pointerEvent(EventKind.PointerMove, 30, 40, 0, 0));
  canvas.dispatchEvent(domEvent("pointermove", { clientX: 30, clientY: 40, pointerId: 2, pointerType: "mouse", getPredictedEvents: ahead }));
  expect(ring.read().events).toEqual(pointerEvent(EventKind.PointerMove, 30, 40, 0, 0));
}));

const bytes = (events: Uint8Array[]) => new Uint8Array(events.flatMap((e) => [...e]));
const chars = (text: string) => [...text].map((c) => textEvent(c.codePointAt(0)!));

test("a group is written whole or not at all", () => {
  const ring = InputRing.create();
  for (let i = 0; i < InputRing.CAPACITY - 1; i++) ring.write(textEvent(i), i);
  expect(ring.writeAll(composeEvents("ab"), 0)).toBe(false);
  ring.read();
  expect(ring.writeAll(composeEvents("ab"), 0)).toBe(true);
  expect(ring.read().events).toEqual(bytes(composeEvents("ab")));
});

test("text mode types from its field, composes in whole groups and leaves editing keys to the program", () => onPage((page) => {
  const canvas = page.createElement("canvas");
  canvas.focus();
  const ring = InputRing.create();
  const input = listen(canvas as unknown as HTMLCanvasElement, ring, () => 1, () => ({ width: 100, height: 100 }));
  input.textInput(true, { x: 10, y: 20, w: 2, h: 16 });
  const field = page.activeElement!;
  expect(field.tagName).toBe("TEXTAREA");
  expect(field.style.top).toBe("20px");
  // A printable key is a KeyDown only: its text comes from the field, spaces included.
  page.dispatchEvent(domEvent("keydown", { code: "KeyA", key: "a" }));
  field.dispatchEvent(domEvent("input", { inputType: "insertText", data: "a b" }));
  expect(ring.read().events).toEqual(bytes([keyEvent(true, keyCode("KeyA"), false, 0), ...chars("a b")]));
  // Enter is the program's KeyDown; the field doesn't also act on it.
  const enter = domEvent("keydown", { code: "Enter", key: "Enter" });
  page.dispatchEvent(enter);
  expect(enter.defaultPrevented).toBe(true);
  const space = domEvent("keydown", { code: "Space", key: " " });
  page.dispatchEvent(space);
  expect(space.defaultPrevented).toBe(false);
  ring.read();
  // While an input method composes, its keys aren't sent; each change arrives whole, and the
  // commit clears the composing before the committed text.
  page.dispatchEvent(domEvent("keydown", { code: "KeyK", key: "Process", isComposing: true, keyCode: 229 }));
  field.dispatchEvent(domEvent("compositionupdate", { data: "かな" }));
  field.dispatchEvent(domEvent("input", { inputType: "insertCompositionText", data: "かな", isComposing: true }));
  field.dispatchEvent(domEvent("compositionend", { data: "仮名" }));
  field.dispatchEvent(domEvent("input", { inputType: "insertCompositionText", data: "仮名" }));
  expect(ring.read().events).toEqual(bytes([...composeEvents("かな"), ...composeEvents(""), ...chars("仮名")]));
  input.textInput(false, { x: 0, y: 0, w: 0, h: 0 });
  expect(page.activeElement).toBe(canvas);
}));

test("a composition that doesn't fit waits whole, and a newer change replaces it", () => onPage((page) => {
  const canvas = page.createElement("canvas");
  const ring = InputRing.create();
  const input = listen(canvas as unknown as HTMLCanvasElement, ring, () => 1, () => ({ width: 100, height: 100 }));
  input.textInput(true, { x: 0, y: 0, w: 1, h: 10 });
  const field = page.activeElement!;
  for (let i = 0; i < InputRing.CAPACITY - 1; i++) ring.write(textEvent(65), i);
  field.dispatchEvent(domEvent("compositionupdate", { data: "ab" }));
  field.dispatchEvent(domEvent("compositionupdate", { data: "abc" }));
  expect(ring.read().events.length).toBe((InputRing.CAPACITY - 1) * EVENT_SIZE);
  field.dispatchEvent(domEvent("compositionend", { data: "abc" }));
  page.frame();
  expect(ring.read().events).toEqual(bytes([...composeEvents("abc"), ...composeEvents(""), ...chars("abc")]));
}));

test("a paste is kept by its serial, and a copy takes what the program offers", () => onPage((page) => {
  const canvas = page.createElement("canvas");
  const ring = InputRing.create();
  const input = listen(canvas as unknown as HTMLCanvasElement, ring, () => 1, () => ({ width: 100, height: 100 }));
  const clipboard = (data: Record<string, string>) => ({
    getData: (t: string) => data[t] ?? "",
    setData: (t: string, v: string) => { data[t] = v; },
  });
  for (let i = 1; i <= 9; i++) {
    const paste = domEvent("paste", { clipboardData: clipboard({ "text/plain": `p${i}`, "text/html": `<i>p${i}</i>` }) });
    page.dispatchEvent(paste);
    expect(paste.defaultPrevented).toBe(true);
  }
  expect(ring.read().events.subarray(0, EVENT_SIZE)).toEqual(pasteEvent(1));
  expect(input.pasted(9)).toEqual({ text: "p9", html: "<i>p9</i>" });
  expect(input.pasted(1)).toBeNull();
  const nothing = domEvent("copy", { clipboardData: clipboard({}) });
  page.dispatchEvent(nothing);
  expect(nothing.defaultPrevented).toBe(false);
  input.offer("note", "<p>note</p>");
  const out: Record<string, string> = {};
  const copy = domEvent("copy", { clipboardData: clipboard(out) });
  page.dispatchEvent(copy);
  expect(copy.defaultPrevented).toBe(true);
  expect(out).toEqual({ "text/plain": "note", "text/html": "<p>note</p>" });
}));

test("F6 hands the keys to the overlay, letting go of held keys first", () => onPage((page) => {
  const canvas = page.createElement("canvas");
  const control = page.createElement("button");
  const overlay = { contains: (e: Element | null) => e === (control as unknown), enter: () => (control.focus(), true), focusAt: () => false, connect() {} };
  const ring = InputRing.create();
  listen(canvas as unknown as HTMLCanvasElement, ring, () => 1, () => ({ width: 100, height: 100 }), overlay);
  page.dispatchEvent(domEvent("keydown", { code: "Space", key: " " }));
  ring.read();
  const f6 = domEvent("keydown", { code: "F6", key: "F6" });
  page.dispatchEvent(f6);
  expect(f6.defaultPrevented).toBe(true);
  expect(ring.read().events).toEqual(keyEvent(false, keyCode("Space"), false, 0));
  // In the overlay, keys are its own: the program hears nothing.
  page.dispatchEvent(domEvent("keyup", { code: "Space", key: " " }));
  page.dispatchEvent(domEvent("keydown", { code: "Enter", key: "Enter" }));
  expect(ring.read().events.length).toBe(0);
}));

test("the browser's find keys are the program's only while it claims them; the program hears them either way", () => onPage((page) => {
  const canvas = page.createElement("canvas");
  const ring = InputRing.create();
  const input = listen(canvas as unknown as HTMLCanvasElement, ring, () => 1, () => ({ width: 100, height: 100 }));
  const find = (fields: Record<string, unknown>) => {
    const e = domEvent("keydown", fields);
    page.dispatchEvent(e);
    return e.defaultPrevented;
  };
  expect(find({ code: "KeyF", key: "f", ctrlKey: true })).toBe(false);
  input.claimFind(true);
  expect([find({ code: "KeyF", key: "f", ctrlKey: true }), find({ code: "KeyG", key: "g", metaKey: true }), find({ code: "F3", key: "F3" })]).toEqual([true, true, true]);
  // Not a find: F alone types, and Ctrl+S is the page's.
  expect([find({ code: "KeyF", key: "f" }), find({ code: "KeyS", key: "s", ctrlKey: true })]).toEqual([false, false]);
  input.claimFind(false);
  expect(find({ code: "KeyF", key: "f", metaKey: true })).toBe(false);
  const keys = ring.read().events;
  expect(keys.length / EVENT_SIZE).toBeGreaterThanOrEqual(7);
}));

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

test("a press on a field the overlay draws gives it the keys; the keys it hands on reach the program", () => onPage((page) => {
  const canvas = page.createElement("canvas");
  const field = page.createElement("input");
  let forward: Forward | null = null;
  const overlay = {
    contains: (e: Element | null) => e === (field as unknown),
    enter: () => false,
    focusAt: (x: number, y: number) => x < 10 && y < 10 && (field.focus(), true),
    connect: (f: Forward) => { forward = f; },
  };
  const ring = InputRing.create();
  listen(canvas as unknown as HTMLCanvasElement, ring, () => 1, () => ({ width: 100, height: 100 }), overlay);
  canvas.dispatchEvent(domEvent("pointerdown", { clientX: 5, clientY: 5, button: 0, pointerId: 1 }));
  expect(page.activeElement).toBe(field);
  canvas.dispatchEvent(domEvent("pointerdown", { clientX: 50, clientY: 50, button: 0, pointerId: 1 }));
  expect(page.activeElement).toBe(canvas);
  ring.read();
  forward!.key(domEvent("keydown", { code: "Enter", key: "Enter" }) as KeyboardEvent, true);
  forward!.key(domEvent("keyup", { code: "Enter", key: "Enter" }) as KeyboardEvent, false);
  expect(ring.read().events).toEqual(new Uint8Array([...keyEvent(true, keyCode("Enter"), false, 0), ...keyEvent(false, keyCode("Enter"), false, 0)]));
}));
