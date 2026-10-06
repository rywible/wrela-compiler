// Input (runtime/abi `input`): events as the program reads them, encoded from DOM events on the
// main thread and from a script in test mode, and the ring that carries them to the render
// worker. The ring is a SharedArrayBuffer, so an event written before a frame starts is in that
// frame: it doesn't wait behind other messages.

import { BUTTONS, EVENT_SIZE, EventKind, KEYS, MODIFIER_ALT, MODIFIER_CONTROL, MODIFIER_META, MODIFIER_SHIFT } from "./abi.gen.ts";

/** One event's EVENT_SIZE bytes: its kind, its modifiers, then four words. */
export function encodeEvent(kind: number, modifiers: number, a: number, b: number, c: number, d: number): Uint8Array {
  const out = new Uint8Array(EVENT_SIZE);
  const view = new DataView(out.buffer);
  [kind, modifiers, a, b, c, d].forEach((w, i) => view.setUint32(4 * i, w >>> 0, true));
  return out;
}

const F32 = new Float32Array(1);
const U32 = new Uint32Array(F32.buffer);
/** An f32's bits. */
export function bits(x: number): number {
  F32[0] = x;
  return U32[0]!;
}

const KEY_CODES = new Map<string, number>(KEYS.map((k, i) => [k, i]));
/** A DOM `code`'s key number; 0 for one the ABI doesn't name. */
export const keyCode = (code: string) => KEY_CODES.get(code) ?? 0;

export function pointerEvent(kind: number, x: number, y: number, button: number, modifiers: number): Uint8Array {
  return encodeEvent(kind, modifiers, bits(x), bits(y), button, 0);
}

export function wheelEvent(x: number, y: number, dx: number, dy: number, modifiers: number): Uint8Array {
  return encodeEvent(EventKind.Wheel, modifiers, bits(x), bits(y), bits(dx), bits(dy));
}

export function keyEvent(down: boolean, key: number, repeat: boolean, modifiers: number): Uint8Array {
  return encodeEvent(down ? EventKind.KeyDown : EventKind.KeyUp, modifiers, key, down && repeat ? 1 : 0, 0, 0);
}

export const textEvent = (codepoint: number) => encodeEvent(EventKind.Text, 0, codepoint, 0, 0, 0);

/** When a script's event arrives: before a frame, or as a tick's record (runtime/abi `input`). */
export type At = { frame: number } | { tick: number };

/** A script's event and when it arrives. */
export interface Scripted {
  at: At;
  event: Uint8Array;
}

/** The events of `script` that arrive before frame `frame`. */
export const eventsAt = (script: Scripted[], frame: number) =>
  script.filter((s) => "frame" in s.at && s.at.frame === frame).map((s) => s.event);

/** The events of `script` that are tick `tick`'s records. */
export const recordsAt = (script: Scripted[], tick: number) =>
  script.filter((s) => "tick" in s.at && s.at.tick === tick).map((s) => s.event);

/** Reads a script (runtime/abi `input`'s format), with the Rust parser's messages. */
export function parseScript(text: string): Scripted[] {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (e) {
    throw new Error(`the script isn't JSON: ${e instanceof Error ? e.message : String(e)}`);
  }
  if (!Array.isArray(value)) throw new Error("a script is a JSON array of events");
  const out: Scripted[] = [];
  let lastFrame = 0;
  let lastTick = 0;
  value.forEach((item: unknown, i: number) => {
    const at = (why: string) => new Error(`event ${i}: ${why}`);
    if (typeof item !== "object" || item === null || Array.isArray(item)) throw at("an event is a JSON object");
    const o = item as Record<string, unknown>;
    const whole = (v: unknown) => typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= 0xffffffff;
    let when: At;
    if (o.frame !== undefined && o.tick !== undefined) {
      throw at(whole(o.frame) && whole(o.tick) ? "an event has a `frame` or a `tick`, not both" : "`frame` (or `tick`) must be a whole number");
    } else if (o.frame !== undefined && whole(o.frame)) {
      const frame = o.frame as number;
      if (frame < lastFrame) throw at("frames don't decrease");
      lastFrame = frame;
      when = { frame };
    } else if (o.tick !== undefined && whole(o.tick)) {
      const tick = o.tick as number;
      if (tick < lastTick) throw at("ticks don't decrease");
      lastTick = tick;
      when = { tick };
    } else {
      throw at("`frame` (or `tick`) must be a whole number");
    }
    const num = (key: string) => {
      const v = o[key];
      if (v === undefined) return 0;
      if (typeof v !== "number" || !Number.isFinite(Math.fround(v))) throw at(`\`${key}\` must be a number`);
      return Math.fround(v);
    };
    const flag = (key: string) => {
      const v = o[key];
      if (v === undefined) return false;
      if (typeof v !== "boolean") throw at(`\`${key}\` must be true or false`);
      return v;
    };
    let modifiers = 0;
    for (const [k, bit] of [["shift", MODIFIER_SHIFT], ["ctrl", MODIFIER_CONTROL], ["alt", MODIFIER_ALT], ["meta", MODIFIER_META]] as const) {
      if (flag(k)) modifiers |= bit;
    }
    const type = o.type;
    if (typeof type !== "string") throw at("`type` is missing");
    const button = () => {
      const v = o.button;
      if (v === undefined) return 0;
      const b = BUTTONS.findIndex((x) => x === v);
      if (b < 0) throw at("`button` is primary, middle or secondary");
      return b;
    };
    const key = () => {
      const name = o.key;
      if (typeof name !== "string") throw at("`key` is missing");
      const k = keyCode(name);
      if (k === 0) throw at(`\`${name}\` isn't a key (they're DOM codes, such as KeyA)`);
      return k;
    };
    const push = (event: Uint8Array) => out.push({ at: when, event });
    switch (type) {
      case "move":
        push(pointerEvent(EventKind.PointerMove, num("x"), num("y"), 0, modifiers));
        break;
      case "down":
      case "up":
        push(pointerEvent(type === "down" ? EventKind.PointerDown : EventKind.PointerUp, num("x"), num("y"), button(), modifiers));
        break;
      case "wheel":
        push(wheelEvent(num("x"), num("y"), num("dx"), num("dy"), modifiers));
        break;
      case "keydown":
        push(keyEvent(true, key(), flag("repeat"), modifiers));
        break;
      case "keyup":
        push(keyEvent(false, key(), false, modifiers));
        break;
      case "key": {
        const k = key();
        push(keyEvent(true, k, false, modifiers));
        push(keyEvent(false, k, false, modifiers));
        break;
      }
      case "text": {
        const t = o.text;
        if (typeof t !== "string") throw at("`text` is missing");
        for (const c of t) {
          const cp = c.codePointAt(0)!;
          // A lone surrogate isn't a Unicode scalar value (serde_json refuses the script too).
          if (cp >= 0xd800 && cp <= 0xdfff) throw at("`text` has a lone surrogate, which isn't a character");
          push(textEvent(cp));
        }
        break;
      }
      default:
        throw at(`\`${type}\` isn't a type (move, down, up, wheel, keydown, keyup, key or text)`);
    }
  });
  return out;
}

/** The modifiers a DOM event holds, as the ABI's bits. */
export function modifiersOf(e: { shiftKey: boolean; ctrlKey: boolean; altKey: boolean; metaKey: boolean }): number {
  return (e.shiftKey ? MODIFIER_SHIFT : 0) | (e.ctrlKey ? MODIFIER_CONTROL : 0) | (e.altKey ? MODIFIER_ALT : 0) | (e.metaKey ? MODIFIER_META : 0);
}

/** A DOM button's number, as the ABI numbers it: 0 primary, 1 middle, 2 secondary. */
export const buttonOf = (button: number) => (button === 1 ? 1 : button === 2 ? 2 : 0);

/**
 * A single-writer ring of events in a SharedArrayBuffer, with two readers: the main thread
 * writes; the render worker's frames read them for the program's `std::input::events()`, and the
 * ticker's thread stamps them into its ticks' records (#43 §2.2). Words: how many events have
 * been written, how many the frames have read, how many the ticker has read, and whether the
 * ticker reads; then CAPACITY events, each with the writer's clock (a float64, ms since the time
 * origin) after it.
 * - The writer drops an event only when the ticker's reader is a full ring behind (or, with no
 *   ticker, the frames' reader): what the ticker reads is the sim's input, which is never lost.
 * - With a ticker, the frames' reader, a full ring behind, skips ahead: a UI loses events, the
 *   sim never does.
 */
export class InputRing {
  static readonly CAPACITY = 1024;
  static readonly SLOT = EVENT_SIZE + 8;
  static readonly HEADER = 16;
  readonly #counts: Int32Array;
  readonly #bytes: Uint8Array;
  readonly #view: DataView;

  constructor(readonly buffer: SharedArrayBuffer) {
    this.#counts = new Int32Array(buffer, 0, 4);
    this.#bytes = new Uint8Array(buffer, InputRing.HEADER);
    this.#view = new DataView(buffer, InputRing.HEADER);
  }

  static create(): InputRing {
    return new InputRing(new SharedArrayBuffer(InputRing.HEADER + InputRing.CAPACITY * InputRing.SLOT));
  }

  /** Writes an event, stamped with `time` (performance.timeOrigin + performance.now()). */
  write(event: Uint8Array, time: number): boolean {
    const written = Atomics.load(this.#counts, 0);
    const slowest = Atomics.load(this.#counts, 3) !== 0 ? Atomics.load(this.#counts, 2) : Atomics.load(this.#counts, 1);
    if (written - slowest >= InputRing.CAPACITY) return false;
    const at = (written % InputRing.CAPACITY) * InputRing.SLOT;
    this.#bytes.set(event, at);
    this.#view.setFloat64(at + EVENT_SIZE, time, true);
    Atomics.store(this.#counts, 0, written + 1);
    return true;
  }

  /** The ticker reads from now on: the writer keeps what it hasn't read. */
  attachTicker(): void {
    Atomics.store(this.#counts, 2, Atomics.load(this.#counts, 0));
    Atomics.store(this.#counts, 3, 1);
  }

  /** Every event written and not yet read by the frames, oldest first, each with its time. A
   * reader that's fallen a full ring behind skips the events written over. */
  read(): { events: Uint8Array; times: number[] } {
    return this.#read(1, true);
  }

  /** Every event written and not yet read by the ticker, at most `cap`, oldest first, each with
   * its time; the rest wait for the next read. */
  readTicker(cap: number): { events: Uint8Array; times: number[] } {
    return this.#read(2, false, cap);
  }

  #read(reader: number, skip: boolean, cap = Infinity): { events: Uint8Array; times: number[] } {
    const written = Atomics.load(this.#counts, 0);
    let read = Atomics.load(this.#counts, reader);
    // A margin, so the slot the writer fills next isn't one this reads.
    const room = InputRing.CAPACITY - 64;
    if (skip && Atomics.load(this.#counts, 3) !== 0 && written - read > room) read = written - room;
    const n = Math.min(written - read, cap);
    const events = new Uint8Array(n * EVENT_SIZE);
    const times: number[] = [];
    for (let i = 0; i < n; i++) {
      const at = ((read + i) % InputRing.CAPACITY) * InputRing.SLOT;
      events.set(this.#bytes.subarray(at, at + EVENT_SIZE), i * EVENT_SIZE);
      times.push(this.#view.getFloat64(at + EVENT_SIZE, true));
    }
    Atomics.store(this.#counts, reader, read + n);
    return { events, times };
  }
}
