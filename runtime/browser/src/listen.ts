// The main thread's half of input (input.ts): the DOM's events, as the ABI's, into the ring the
// render worker reads.

import { EventKind, PADS } from "./abi.gen.ts";
import {
  buttonOf,
  type InputRing,
  keyCode,
  keyEvent,
  modifiersOf,
  padAxisEvent,
  padButtonEvent,
  pointerEvent,
  textEvent,
  wheelEvent,
} from "./input.ts";

/**
 * Turns the DOM's pointer, wheel and keyboard events on `canvas` (keys anywhere on the page)
 * into events in `ring`. Positions are in the canvas's device pixels, as `frame`'s width and
 * height count them. A key's text arrives as a Text event, after the key's KeyDown.
 */
export function listen(
  canvas: HTMLCanvasElement,
  ring: InputRing,
  now: () => number,
  size: () => { width: number; height: number },
): void {
  const at = (e: MouseEvent) => {
    const r = canvas.getBoundingClientRect();
    const { width, height } = size();
    const sx = r.width > 0 ? width / r.width : 1;
    const sy = r.height > 0 ? height / r.height : 1;
    return [(e.clientX - r.left) * sx, (e.clientY - r.top) * sy] as const;
  };
  const put = (event: Uint8Array) => ring.write(event, now());
  canvas.addEventListener("pointermove", (e) => {
    // Coalesced moves arrive as one: the program needs where the pointer is, not each step.
    const [x, y] = at(e);
    put(pointerEvent(EventKind.PointerMove, x, y, 0, modifiersOf(e)));
  });
  canvas.addEventListener("pointerdown", (e) => {
    canvas.setPointerCapture(e.pointerId);
    const [x, y] = at(e);
    put(pointerEvent(EventKind.PointerDown, x, y, buttonOf(e.button), modifiersOf(e)));
    e.preventDefault();
  });
  canvas.addEventListener("pointerup", (e) => {
    const [x, y] = at(e);
    put(pointerEvent(EventKind.PointerUp, x, y, buttonOf(e.button), modifiersOf(e)));
  });
  canvas.addEventListener("contextmenu", (e) => e.preventDefault());
  canvas.addEventListener(
    "wheel",
    (e) => {
      const [x, y] = at(e);
      // Lines and pages become pixels: a line is 16 px, a page the canvas's height.
      const k = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? size().height : 1;
      put(wheelEvent(x, y, e.deltaX * k, e.deltaY * k, modifiersOf(e)));
      e.preventDefault();
    },
    { passive: false },
  );
  window.addEventListener("keydown", (e) => {
    put(keyEvent(true, keyCode(e.code), e.repeat, modifiersOf(e)));
    // A printable key types its text, unless Control or Meta make it a shortcut.
    if (e.key.length > 0 && [...e.key].length === 1 && !e.ctrlKey && !e.metaKey) {
      put(textEvent(e.key.codePointAt(0)!));
    }
    // Keys the page would act on (Tab moves focus, Space scrolls, Backspace goes back).
    if (["Tab", "Space", "Backspace", "ArrowUp", "ArrowDown", "PageUp", "PageDown"].includes(e.code)) e.preventDefault();
  });
  window.addEventListener("keyup", (e) => {
    put(keyEvent(false, keyCode(e.code), false, modifiersOf(e)));
  });
}

/** A pad's buttons and axes as last sent: each button's value and pressed state, each axis's
 * value. */
export interface PadState {
  values: number[];
  pressed: boolean[];
  axes: number[];
}

/** What a pad reports, as the Gamepad API has it (a `Gamepad`, or a test's stand-in): its
 * `mapping` is "standard" when the browser gives its buttons and axes in the standard layout,
 * "" when they're the device's own numbers. */
export interface PadReading {
  connected: boolean;
  mapping: string;
  buttons: readonly { value: number; pressed: boolean }[];
  axes: readonly number[];
}

/** A value as a pad's events carry it: in steps of 1/1024, so a stick's noise below that sends
 * nothing. */
const step = (v: number) => Math.round(v * 1024) / 1024;

/**
 * The events for what the pads changed since `last` (each pad as last sent, null if it wasn't
 * there), which it updates: an event for each button whose value or pressed state changed, and
 * each axis whose value did. Pads 0 to 3, in the standard mapping's numbers; a pad the browser
 * can't give in that mapping sends nothing (its own numbers would be other buttons), and a pad
 * that goes lets go of what it held.
 */
export function padChanges(pads: readonly (PadReading | null)[], last: (PadState | null)[]): Uint8Array[] {
  const out: Uint8Array[] = [];
  for (let i = 0; i < PADS; i++) {
    const pad = pads[i] ?? null;
    const was = last[i] ?? null;
    if (pad === null || !pad.connected || pad.mapping !== "standard") {
      if (was !== null) {
        for (let b = 0; b < was.pressed.length; b++) {
          if (was.pressed[b] || was.values[b] !== 0) out.push(padButtonEvent(i, b, 0, false));
        }
        for (let a = 0; a < was.axes.length; a++) {
          if (was.axes[a] !== 0) out.push(padAxisEvent(i, a, 0));
        }
        last[i] = null;
      }
      continue;
    }
    const state: PadState = was ?? { values: [], pressed: [], axes: [] };
    pad.buttons.forEach((button, b) => {
      const value = step(Math.min(Math.max(button.value, 0), 1));
      if (value !== (state.values[b] ?? 0) || button.pressed !== (state.pressed[b] ?? false)) {
        out.push(padButtonEvent(i, b, value, button.pressed));
        state.values[b] = value;
        state.pressed[b] = button.pressed;
      }
    });
    pad.axes.forEach((raw, a) => {
      const value = step(Math.min(Math.max(raw, -1), 1));
      if (value !== (state.axes[a] ?? 0)) {
        out.push(padAxisEvent(i, a, value));
        state.axes[a] = value;
      }
    });
    last[i] = state;
  }
  return out;
}

/** The gamepads' changes into `ring` (`padChanges`), polled each animation frame: the Gamepad
 * API has no events for them. */
export function pollPads(ring: InputRing, now: () => number): void {
  if (typeof navigator.getGamepads !== "function") return;
  const last: (PadState | null)[] = Array.from({ length: PADS }, () => null);
  const poll = () => {
    for (const event of padChanges(navigator.getGamepads(), last)) ring.write(event, now());
    requestAnimationFrame(poll);
  };
  requestAnimationFrame(poll);
}
