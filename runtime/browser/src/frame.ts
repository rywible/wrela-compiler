// What each frame of a normal run is given (worker.ts): the time, and the canvas size.

/** Seconds of visible running: the time stops while the page is hidden. */
export class GameClock {
  #elapsed = 0;
  /** The last frame's timestamp while visible; null before the first visible frame. */
  #last: number | null = null;
  #visible = true;

  /** The page was shown or hidden. A browser may run no frames while the page is hidden, or
   * keep running them, so the time stops here, not at the next frame. */
  setVisible(visible: boolean): void {
    this.#visible = visible;
    this.#last = null;
  }

  /** The time for a frame at timestamp `now` (milliseconds), or null while the page is hidden
   * (run no frame). */
  tick(now: number): number | null {
    if (!this.#visible) return null;
    if (this.#last !== null) this.#elapsed += now - this.#last;
    this.#last = now;
    return this.#elapsed / 1000;
  }
}

/** A canvas size in device pixels that fits the device's largest texture (`max` on each
 * side): scaled down as a whole when it doesn't, so the picture keeps its shape. */
export function fitSize(width: number, height: number, max: number): { width: number; height: number } {
  const w = Math.max(width, 1);
  const h = Math.max(height, 1);
  const scale = Math.min(1, max / Math.max(w, h));
  return {
    width: Math.min(Math.max(Math.round(w * scale), 1), max),
    height: Math.min(Math.max(Math.round(h * scale), 1), max),
  };
}
