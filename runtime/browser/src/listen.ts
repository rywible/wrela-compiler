// The main thread's half of input (input.ts): the DOM's events, as the ABI's, into the ring the
// render worker reads, and the services that go with them: text mode, pastes and copies.

import { EventKind, PAD_AXES, PAD_BUTTONS, PADS } from "./abi.gen.ts";
import {
  buttonOf,
  composeEvents,
  type InputRing,
  keyCode,
  keyEvent,
  modifiersOf,
  padAxisEvent,
  padButtonEvent,
  pasteEvent,
  pointerEvent,
  textEvent,
  wheelEvent,
} from "./input.ts";

/** A rectangle in the canvas's device pixels. */
export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** What a paste carried. */
export interface Pasted {
  text: string;
  html: string;
}

/** The input services `listen` gives the host (platform.ts). */
export interface Listener {
  /** Lets go of every key and pointer button the program holds, as if they were released. */
  release(): void;
  /** Text mode on, with the caret at `caret`, or off: the canvas has the keys again. */
  textInput(active: boolean, caret: Box): void;
  /** What paste `serial` carried, while it's kept (the last few). */
  pasted(serial: number): Pasted | null;
  /** What a copy or cut puts on the clipboard from now on (empty texts: nothing). */
  offer(text: string, html: string): void;
  /** Whether the program takes the browser's find keys (Ctrl or Cmd with F or G, and F3) for a
   * find of its own: the browser's finds nothing in a canvas. */
  claimFind(on: boolean): void;
}

/** Whether `e` is one of the browser's find keys. */
export const isFindKey = (e: KeyboardEvent): boolean =>
  e.code === "F3" || ((e.ctrlKey || e.metaKey) && (e.code === "KeyF" || e.code === "KeyG"));

/** How many pastes are kept for the program to ask for. */
const PASTES_KEPT = 8;

/** Keys the program edits text with: in text mode the hidden field mustn't act on them too. */
const EDITING = new Set([
  "Enter", "NumpadEnter", "Backspace", "Delete", "Tab", "Escape", "ArrowLeft", "ArrowRight",
  "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown",
]);

/**
 * Turns the DOM's pointer, wheel, keyboard, composition and paste events (keys anywhere on the
 * page) into events in `ring`. Positions are in the canvas's device pixels, as `frame`'s width
 * and height count them.
 *
 * Keys have one owner at a time:
 * - **the canvas** (the default): each key's KeyDown, then the text it types as Text; the keys
 *   the page would act on (Tab, Space, Backspace, scrolling) are the program's;
 * - **text mode** (`textInput`, while the program edits text): a hidden field at the caret takes
 *   the keys, so an input method composes there. Typed text comes from the field (Text), a
 *   composition's changes as Compose groups, and committed text as a group that clears the
 *   composing first; editing keys (Enter, Backspace, the arrows) are KeyDowns the field doesn't
 *   act on, and keys an input method is using aren't sent at all;
 * - **the overlay** (`overlay`, the accessibility tree, assist.ts): its own elements take the keys
 *   and none reach the program. F6 from the canvas enters it, letting go of held keys first.
 */
export function listen(
  canvas: HTMLCanvasElement,
  ring: InputRing,
  now: () => number,
  size: () => { width: number; height: number },
  overlay: { contains(e: Element | null): boolean; enter(): boolean } | null = null,
): Listener {
  const at = (e: MouseEvent) => {
    const r = canvas.getBoundingClientRect();
    const { width, height } = size();
    const sx = r.width > 0 ? width / r.width : 1;
    const sy = r.height > 0 ? height / r.height : 1;
    return [(e.clientX - r.left) * sx, (e.clientY - r.top) * sy] as const;
  };
  const put = (event: Uint8Array) => ring.write(event, now());
  const keys = new Set<number>();
  const pointers = new Map<number, { x: number; y: number; buttons: Set<number> }>();
  const releasePointer = (id: number) => {
    const pointer = pointers.get(id);
    if (!pointer) return;
    for (const button of pointer.buttons) put(pointerEvent(EventKind.PointerUp, pointer.x, pointer.y, button, 0));
    pointers.delete(id);
  };
  const release = () => {
    for (const key of keys) put(keyEvent(false, key, false, 0));
    keys.clear();
    for (const id of pointers.keys()) releasePointer(id);
  };

  // Groups that must arrive whole (a composition's change, a commit), in order: one that doesn't
  // fit waits for room, and a newer change replaces a change still waiting; commits are kept.
  const groups: { events: Uint8Array[]; change: boolean }[] = [];
  let retrying = false;
  const flush = () => {
    while (groups.length > 0 && ring.writeAll(groups[0]!.events, now())) groups.shift();
    if (groups.length > 0 && !retrying) {
      retrying = true;
      requestAnimationFrame(() => {
        retrying = false;
        flush();
      });
    }
  };
  const group = (events: Uint8Array[], change: boolean) => {
    if (change && groups.length > 0 && groups[groups.length - 1]!.change) groups.pop();
    groups.push({ events, change });
    flush();
  };

  // Text mode's field: invisible, at the caret so an input method's window opens there; made
  // when the program first asks for text mode.
  let field: HTMLTextAreaElement | null = null;
  const texting = () => field !== null && document.activeElement === field;
  const inOverlay = () => overlay !== null && overlay.contains(document.activeElement);
  const makeField = () => {
    const f = document.createElement("textarea");
    f.setAttribute("aria-label", "Text being edited");
    f.autocomplete = "off";
    f.spellcheck = false;
    f.setAttribute("autocapitalize", "off");
    f.style.cssText = "position:fixed;left:0;top:0;width:1px;height:1em;padding:0;border:0;margin:0;" +
      "opacity:0;resize:none;overflow:hidden;pointer-events:none;z-index:0;color:transparent;background:transparent";
    document.body.append(f);
    f.addEventListener("input", (e) => {
      const input = e as InputEvent;
      if (input.isComposing) return;
      // Typed text; a composition's text is committed at its end, and editing keys are KeyDowns.
      if ((input.inputType === "insertText" || input.inputType === "insertReplacementText") && input.data) {
        group([...input.data].map((c) => textEvent(c.codePointAt(0)!)), false);
      }
      f.value = "";
    });
    f.addEventListener("compositionupdate", (e) => group(composeEvents(e.data ?? ""), true));
    f.addEventListener("compositionend", (e) => {
      const committed = [...(e.data ?? "")].map((c) => textEvent(c.codePointAt(0)!));
      group([...composeEvents(""), ...committed], false);
      f.value = "";
    });
    return f;
  };

  // Pastes, kept by serial for the program to ask for (`clipboard/pasted`).
  const pastes = new Map<number, Pasted>();
  let serial = 0;
  window.addEventListener("paste", (e) => {
    if (inOverlay() || !e.clipboardData) return;
    e.preventDefault();
    serial += 1;
    pastes.set(serial, { text: e.clipboardData.getData("text/plain"), html: e.clipboardData.getData("text/html") });
    pastes.delete(serial - PASTES_KEPT);
    put(pasteEvent(serial));
  });
  // A copy or cut takes what the program offers: it offers before the keys that copy arrive.
  let offered: Pasted = { text: "", html: "" };
  const copy = (e: ClipboardEvent) => {
    if (inOverlay() || !e.clipboardData || offered.text === "") return;
    e.clipboardData.setData("text/plain", offered.text);
    if (offered.html !== "") e.clipboardData.setData("text/html", offered.html);
    e.preventDefault();
  };
  window.addEventListener("copy", copy);
  window.addEventListener("cut", copy);

  canvas.addEventListener("pointermove", (e) => {
    // Coalesced moves arrive as one: the program needs where the pointer is, not each step.
    const [x, y] = at(e);
    const pointer = pointers.get(e.pointerId);
    if (pointer) Object.assign(pointer, { x, y });
    put(pointerEvent(EventKind.PointerMove, x, y, 0, modifiersOf(e)));
  });
  canvas.addEventListener("pointerdown", (e) => {
    canvas.setPointerCapture(e.pointerId);
    const [x, y] = at(e);
    const button = buttonOf(e.button);
    const pointer = pointers.get(e.pointerId) ?? { x, y, buttons: new Set<number>() };
    Object.assign(pointer, { x, y });
    pointer.buttons.add(button);
    pointers.set(e.pointerId, pointer);
    put(pointerEvent(EventKind.PointerDown, x, y, button, modifiersOf(e)));
    e.preventDefault();
    // A press on the canvas gives it the keys back from the overlay; text mode keeps its field.
    if (!texting()) canvas.focus({ preventScroll: true });
  });
  canvas.addEventListener("pointerup", (e) => {
    const [x, y] = at(e);
    const button = buttonOf(e.button);
    const pointer = pointers.get(e.pointerId);
    pointer?.buttons.delete(button);
    if (pointer?.buttons.size === 0) pointers.delete(e.pointerId);
    put(pointerEvent(EventKind.PointerUp, x, y, button, modifiersOf(e)));
  });
  canvas.addEventListener("pointercancel", (e) => releasePointer(e.pointerId));
  canvas.addEventListener("lostpointercapture", (e) => releasePointer(e.pointerId));
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
  let findClaimed = false;
  window.addEventListener("keydown", (e) => {
    if (inOverlay()) return;
    // The program's own find, rather than the browser's.
    if (findClaimed && isFindKey(e)) e.preventDefault();
    if (texting()) {
      // An input method's keys are its own.
      if (e.isComposing || e.keyCode === 229) return;
      const key = keyCode(e.code);
      keys.add(key);
      put(keyEvent(true, key, e.repeat, modifiersOf(e)));
      if (EDITING.has(e.code)) e.preventDefault();
      return;
    }
    if (e.code === "F6" && overlay !== null) {
      release();
      if (overlay.enter()) {
        e.preventDefault();
        return;
      }
    }
    const key = keyCode(e.code);
    keys.add(key);
    put(keyEvent(true, key, e.repeat, modifiersOf(e)));
    // A printable key types its text, unless Control or Meta make it a shortcut.
    if (e.key.length > 0 && [...e.key].length === 1 && !e.ctrlKey && !e.metaKey) {
      put(textEvent(e.key.codePointAt(0)!));
    }
    // Keys the page would act on (Tab moves focus, Space scrolls, Backspace goes back).
    if (["Tab", "Space", "Backspace", "ArrowUp", "ArrowDown", "PageUp", "PageDown"].includes(e.code)) e.preventDefault();
  });
  window.addEventListener("keyup", (e) => {
    if (inOverlay()) return;
    const key = keyCode(e.code);
    keys.delete(key);
    put(keyEvent(false, key, false, modifiersOf(e)));
  });
  window.addEventListener("blur", release);

  return {
    release,
    claimFind(on) {
      findClaimed = on;
    },
    textInput(active, caret) {
      if (active) {
        field ??= makeField();
        const r = canvas.getBoundingClientRect();
        const { width, height } = size();
        const sx = width > 0 ? r.width / width : 1;
        const sy = height > 0 ? r.height / height : 1;
        Object.assign(field.style, {
          left: `${r.left + caret.x * sx}px`,
          top: `${r.top + caret.y * sy}px`,
          height: `${Math.max(caret.h * sy, 1)}px`,
        });
        if (!texting()) field.focus({ preventScroll: true });
      } else if (texting()) {
        canvas.focus({ preventScroll: true });
      }
    },
    pasted: (n) => pastes.get(n) ?? null,
    offer(text, html) {
      offered = { text, html };
    },
  };
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
          if (was.pressed[b] || (was.values[b] ?? 0) !== 0) out.push(padButtonEvent(i, b, 0, false));
        }
        for (let a = 0; a < was.axes.length; a++) {
          if ((was.axes[a] ?? 0) !== 0) out.push(padAxisEvent(i, a, 0));
        }
        last[i] = null;
      }
      continue;
    }
    const state: PadState = was ?? { values: [], pressed: [], axes: [] };
    for (let b = 0; b < PAD_BUTTONS.length; b++) {
      const button = pad.buttons[b];
      const value = button ? step(Math.min(Math.max(button.value, 0), 1)) : 0;
      const pressed = button?.pressed ?? false;
      if (value !== (state.values[b] ?? 0) || pressed !== (state.pressed[b] ?? false)) {
        out.push(padButtonEvent(i, b, value, pressed));
        state.values[b] = value;
        state.pressed[b] = pressed;
      }
    }
    for (let a = 0; a < PAD_AXES.length; a++) {
      const raw = pad.axes[a];
      const value = raw === undefined ? 0 : step(Math.min(Math.max(raw, -1), 1));
      if (value !== (state.axes[a] ?? 0)) {
        out.push(padAxisEvent(i, a, value));
        state.axes[a] = value;
      }
    }
    last[i] = state;
  }
  return out;
}

/** The gamepads' changes into `ring` (`padChanges`), polled each animation frame: the Gamepad
 * API has no events for them. */
export function pollPads(ring: InputRing, now: () => number, active: () => boolean = () => true): void {
  if (typeof navigator.getGamepads !== "function") return;
  const last: (PadState | null)[] = Array.from({ length: PADS }, () => null);
  const poll = () => {
    if (active()) for (const event of padChanges(navigator.getGamepads(), last)) ring.write(event, now());
    requestAnimationFrame(poll);
  };
  requestAnimationFrame(poll);
}
