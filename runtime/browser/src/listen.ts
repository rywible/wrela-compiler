// The main thread's half of input (input.ts): the DOM's events, as the ABI's, into the ring the
// render worker reads.

import { EventKind } from "./abi.gen.ts";
import { buttonOf, type InputRing, keyCode, keyEvent, modifiersOf, pointerEvent, textEvent, wheelEvent } from "./input.ts";

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
