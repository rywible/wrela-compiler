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

/** A script's event and the frame it arrives before. */
export interface Scripted {
  frame: number;
  event: Uint8Array;
}

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
  let last = 0;
  value.forEach((item: unknown, i: number) => {
    const at = (why: string) => new Error(`event ${i}: ${why}`);
    if (typeof item !== "object" || item === null || Array.isArray(item)) throw at("an event is a JSON object");
    const o = item as Record<string, unknown>;
    const frame = o.frame;
    if (typeof frame !== "number" || !Number.isInteger(frame) || frame < 0 || frame > 0xffffffff) {
      throw at("`frame` must be a whole number");
    }
    if (frame < last) throw at("frames don't decrease");
    last = frame;
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
    const push = (event: Uint8Array) => out.push({ frame, event });
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
 * A single-writer, single-reader ring of events in a SharedArrayBuffer: the main thread writes,
 * the render worker reads. Word 0 is how many events have been written, word 1 how many read;
 * then CAPACITY events, each with the writer's clock (a float64, ms since the time origin)
 * after it. A full ring drops what's written (a frame that long gone needs no more pointer moves).
 */
export class InputRing {
  static readonly CAPACITY = 1024;
  static readonly SLOT = EVENT_SIZE + 8;
  readonly #counts: Int32Array;
  readonly #bytes: Uint8Array;
  readonly #view: DataView;

  constructor(readonly buffer: SharedArrayBuffer) {
    this.#counts = new Int32Array(buffer, 0, 2);
    this.#bytes = new Uint8Array(buffer, 8);
    this.#view = new DataView(buffer, 8);
  }

  static create(): InputRing {
    return new InputRing(new SharedArrayBuffer(8 + InputRing.CAPACITY * InputRing.SLOT));
  }

  /** Writes an event, stamped with `time` (performance.timeOrigin + performance.now()). */
  write(event: Uint8Array, time: number): boolean {
    const written = Atomics.load(this.#counts, 0);
    const read = Atomics.load(this.#counts, 1);
    if (written - read >= InputRing.CAPACITY) return false;
    const at = (written % InputRing.CAPACITY) * InputRing.SLOT;
    this.#bytes.set(event, at);
    this.#view.setFloat64(at + EVENT_SIZE, time, true);
    Atomics.store(this.#counts, 0, written + 1);
    return true;
  }

  /** Every event written so far and not yet read, oldest first, each with its time. */
  read(): { events: Uint8Array; times: number[] } {
    const written = Atomics.load(this.#counts, 0);
    const read = Atomics.load(this.#counts, 1);
    const n = written - read;
    const events = new Uint8Array(n * EVENT_SIZE);
    const times: number[] = [];
    for (let i = 0; i < n; i++) {
      const at = ((read + i) % InputRing.CAPACITY) * InputRing.SLOT;
      events.set(this.#bytes.subarray(at, at + EVENT_SIZE), i * EVENT_SIZE);
      times.push(this.#view.getFloat64(at + EVENT_SIZE, true));
    }
    Atomics.store(this.#counts, 1, written);
    return { events, times };
  }
}
