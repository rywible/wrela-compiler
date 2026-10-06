// Input events (src/input.ts): their encoding, and the ring the main thread writes them into.

import { expect, test } from "bun:test";
import { EVENT_SIZE } from "../src/abi.gen.ts";
import { InputRing, keyCode, parseScript, pointerEvent, textEvent } from "../src/input.ts";

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

test("text is characters: a lone surrogate is refused, as the native host refuses it", () => {
  expect(parseScript('[{"frame": 0, "type": "text", "text": "a\\ud83d\\ude00"}]').length).toBe(2);
  expect(() => parseScript('[{"frame": 0, "type": "text", "text": "\\ud800"}]')).toThrow("lone surrogate");
});

test("keys are the ABI's numbers", () => {
  expect(keyCode("KeyA")).toBe(1);
  expect(keyCode("Tab")).toBe(40);
  expect(keyCode("IntlRo")).toBe(0);
});
