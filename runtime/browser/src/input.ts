// Input (runtime/abi `input`): events as the program reads them, encoded from DOM events on the
// main thread and from a script in test mode, and the ring that carries them to the render
// worker. The ring is a SharedArrayBuffer, so an event written before a frame starts is in that
// frame: it doesn't wait behind other messages.

import { BUTTONS, EVENT_SIZE, EventKind, KEYS, MODIFIER_ALT, MODIFIER_CONTROL, MODIFIER_META, MODIFIER_SHIFT, PAD_AXES, PAD_BUTTONS, PADS } from "./abi.gen.ts";

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

/** A gamepad's button (the standard mapping's number), its value (0 to 1) and whether it's
 * pressed. */
export function padButtonEvent(pad: number, button: number, value: number, pressed: boolean): Uint8Array {
  return encodeEvent(EventKind.PadButton, 0, pad, button, bits(value), pressed ? 1 : 0);
}

/** A gamepad's axis (the standard mapping's number) and its value (-1 to 1). */
export function padAxisEvent(pad: number, axis: number, value: number): Uint8Array {
  return encodeEvent(EventKind.PadAxis, 0, pad, axis, bits(value), 0);
}

/** When a script's event arrives: before a frame, or as a tick's record (runtime/abi `input`). */
export type At = { frame: number } | { tick: number };

/** A message from connection `conn`, of `length` bytes, its serial `serial` (runtime/abi
 * `input`'s Messages). */
export const messageEvent = (conn: number, serial: number, length: number) =>
  encodeEvent(EventKind.Message, 0, conn, serial % 2 ** 32, Math.floor(serial / 2 ** 32), length);

/** Connection `conn` opened, or closed. */
export const connectionEvent = (conn: number, open: boolean) => encodeEvent(EventKind.Connection, 0, conn, open ? 1 : 0, 0, 0);

/** Whether an event (its bytes) is a network event: a message or a connection's. */
export function isNetwork(event: Uint8Array): boolean {
  const kind = new DataView(event.buffer, event.byteOffset, 4).getUint32(0, true);
  return kind === EventKind.Message || kind === EventKind.Connection;
}

/** A script's event and when it arrives; a message's bytes (its serial is given when it's
 * queued). */
export interface Scripted {
  at: At;
  event: Uint8Array;
  payload?: Uint8Array;
}

/** A script's events by when they arrive, each list oldest first: by the frame they arrive
 * before, and by the tick whose records they are. */
export interface Arrivals {
  frames: Map<number, Scripted[]>;
  ticks: Map<number, Scripted[]>;
}

/** The events of `script` by when they arrive: grouped once, so a frame or tick finds its own at
 * once. */
export function arrivals(script: Scripted[]): Arrivals {
  const frames = new Map<number, Scripted[]>();
  const ticks = new Map<number, Scripted[]>();
  for (const s of script) {
    const [map, key] = "frame" in s.at ? [frames, s.at.frame] : [ticks, s.at.tick];
    const list = map.get(key);
    if (list === undefined) map.set(key, [s]);
    else list.push(s);
  }
  return { frames, ticks };
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
    const pad = () => {
      const v = o.pad;
      if (v === undefined) return 0;
      if (typeof v !== "number" || !Number.isInteger(v) || v < 0 || v >= PADS) throw at("`pad` is 0, 1, 2 or 3");
      return v;
    };
    const named = (key: string, names: readonly string[]) => {
      const name = o[key];
      if (typeof name !== "string") throw at(`\`${key}\` is missing`);
      const i = names.indexOf(name);
      if (i < 0) throw at(`\`${name}\` isn't a pad's ${key} (${names.join(", ")})`);
      return i;
    };
    const push = (event: Uint8Array) => out.push({ at: when, event });
    // A connection's number: 0 if it's left out.
    const conn = () => {
      if (o.conn === undefined) return 0;
      if (!whole(o.conn)) throw at("`conn` must be a whole number");
      return o.conn as number;
    };
    if ((type === "message" || type === "connection") && "tick" in when) {
      throw at("a tick has no network events: give a message or a connection a `frame`");
    }
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
      case "padbutton": {
        const [p, b] = [pad(), named("button", PAD_BUTTONS)];
        if (o.pressed === undefined) throw at("`pressed` is missing");
        if (typeof o.pressed !== "boolean") throw at("`pressed` must be true or false");
        const value = o.value === undefined ? (o.pressed ? 1 : 0) : num("value");
        if (!(value >= 0 && value <= 1)) throw at("a button's `value` is from 0 to 1");
        push(padButtonEvent(p, b, value, o.pressed));
        break;
      }
      case "padpress": {
        const [p, b] = [pad(), named("button", PAD_BUTTONS)];
        push(padButtonEvent(p, b, 1, true));
        push(padButtonEvent(p, b, 0, false));
        break;
      }
      case "padaxis": {
        const [p, a] = [pad(), named("axis", PAD_AXES)];
        if (o.value === undefined) throw at("`value` is missing");
        const value = num("value");
        if (!(value >= -1 && value <= 1)) throw at("an axis's `value` is from -1 to 1");
        push(padAxisEvent(p, a, value));
        break;
      }
      case "message": {
        const c = conn();
        let payload: Uint8Array;
        if (o.bytes !== undefined && o.text === undefined) {
          const b = o.bytes;
          if (!Array.isArray(b) || !b.every((x) => typeof x === "number" && Number.isInteger(x) && x >= 0 && x < 256)) {
            throw at("`bytes` is an array of numbers from 0 to 255");
          }
          payload = new Uint8Array(b as number[]);
        } else if (o.text !== undefined && o.bytes === undefined) {
          if (typeof o.text !== "string") throw at("`text` must be a string");
          payload = new TextEncoder().encode(o.text);
        } else {
          throw at("a message has `bytes` or `text`");
        }
        out.push({ at: when, event: messageEvent(c, 0, payload.length), payload });
        break;
      }
      case "connection": {
        const c = conn();
        if (o.open === undefined) throw at("`open` is missing");
        if (typeof o.open !== "boolean") throw at("`open` must be true or false");
        push(connectionEvent(c, o.open));
        break;
      }
      default:
        throw at(`\`${type}\` isn't a type (move, down, up, wheel, keydown, keyup, key, text, padbutton, padpress, padaxis, message or connection)`);
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

/** The clock events are stamped with, which every thread shares: ms since 1970. */
export const epochNow = () => performance.timeOrigin + performance.now();

/** What a read of no events gives: nothing is allocated for it. */
const NO_EVENTS: { events: Uint8Array; times: readonly number[] } = Object.freeze({
  events: new Uint8Array(0),
  times: Object.freeze([]),
});

/**
 * A single-writer ring of events in a SharedArrayBuffer, with two readers: the main thread
 * writes; the render worker's frames read them for the program's `std::input::events()`, and the
 * ticker's thread stamps them into its ticks' records (#43 §2.2). Words: how many events have
 * been written, how many the frames have read, how many the ticker has read, and whether the
 * ticker reads; then CAPACITY events, each with the writer's clock (a float64, ms since 1970,
 * `epochNow`) after it.
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

  /** Writes an event, stamped with `time` (`epochNow`). */
  write(event: Uint8Array, time: number): boolean {
    const written = Atomics.load(this.#counts, 0) >>> 0;
    const slowest = (Atomics.load(this.#counts, 3) !== 0 ? Atomics.load(this.#counts, 2) : Atomics.load(this.#counts, 1)) >>> 0;
    if (((written - slowest) >>> 0) >= InputRing.CAPACITY) return false;
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

  /** The running program has no ticker: only frames hold the writer back from now on. */
  detachTicker(): void {
    Atomics.store(this.#counts, 3, 0);
  }

  /** Every event written and not yet read by the frames, oldest first, each with its time. A
   * reader that's fallen a full ring behind skips the events written over. */
  read(): { events: Uint8Array; times: readonly number[] } {
    const written = Atomics.load(this.#counts, 0) >>> 0;
    let read = Atomics.load(this.#counts, 1) >>> 0;
    // With a ticker, leave a margin so the slot the writer fills next isn't one this reads.
    // After it detaches, frames may still be behind events already overwritten by the ticker.
    const room = InputRing.CAPACITY - (Atomics.load(this.#counts, 3) !== 0 ? 64 : 0);
    if (((written - read) >>> 0) > room) read = (written - room) >>> 0;
    const n = (written - read) >>> 0;
    if (n === 0) return NO_EVENTS;
    const events = new Uint8Array(n * EVENT_SIZE);
    this.#copy(read, n, events, 0);
    const times = Array.from({ length: n }, (_, i) => this.#view.getFloat64(this.#slot(read + i) + EVENT_SIZE, true));
    Atomics.store(this.#counts, 1, read + n);
    return { events, times };
  }

  /** Copies the events written and not yet read by the ticker, at most `cap`, oldest first, into
   * `out` from byte `at`; the rest wait for the next read. Returns how many. */
  readTicker(out: Uint8Array, at: number, cap: number): number {
    const read = Atomics.load(this.#counts, 2) >>> 0;
    const n = Math.min(((Atomics.load(this.#counts, 0) >>> 0) - read) >>> 0, cap);
    this.#copy(read, n, out, at);
    Atomics.store(this.#counts, 2, read + n);
    return n;
  }

  /** Where event `i` (counted from the first written) is in the ring's bytes. */
  #slot(i: number): number {
    return (i % InputRing.CAPACITY) * InputRing.SLOT;
  }

  /** Copies `n` events, from event `from` on, into `out` from byte `at`. */
  #copy(from: number, n: number, out: Uint8Array, at: number): void {
    for (let i = 0; i < n; i++) {
      const slot = this.#slot(from + i);
      out.set(this.#bytes.subarray(slot, slot + EVENT_SIZE), at + i * EVENT_SIZE);
    }
  }
}
