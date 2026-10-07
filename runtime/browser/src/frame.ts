// What each frame of a normal run is given (worker.ts): the time, and the canvas size.

/**
 * Visible time: ms since 1970, less the time the page was hidden. A browser may run no frames
 * while the page is hidden, or keep running them, so the time stops when the page hides, not at
 * the next frame. The render worker writes it, as the page shows and hides; the ticker's thread
 * reads it too (#43 §2.1), so a frame's time and the ticks' clock agree. It's in a
 * SharedArrayBuffer: words, a sequence count (odd while it's written) and whether the page is
 * visible; then float64s, the hidden time before the page last hid, when it hid, and the visible
 * time the program started at, which frames and ticks count from.
 */
export class VisibleClock {
  static readonly BYTES = 32;
  readonly #words: Int32Array;
  readonly #floats: Float64Array;

  constructor(readonly buffer: SharedArrayBuffer) {
    this.#words = new Int32Array(buffer, 0, 2);
    this.#floats = new Float64Array(buffer, 8, 3);
  }

  static create(): VisibleClock {
    const clock = new VisibleClock(new SharedArrayBuffer(VisibleClock.BYTES));
    Atomics.store(clock.#words, 1, 1);
    return clock;
  }

  /** The page was shown or hidden at `now` (ms since 1970). Only one thread calls it. */
  setVisible(visible: boolean, now: number): void {
    if (visible === this.visible) return;
    Atomics.add(this.#words, 0, 1);
    if (visible) this.#floats[0]! += now - this.#floats[1]!;
    else this.#floats[1] = now;
    Atomics.store(this.#words, 1, visible ? 1 : 0);
    Atomics.add(this.#words, 0, 1);
  }

  get visible(): boolean {
    return Atomics.load(this.#words, 1) !== 0;
  }

  /** Visible time at `now` (ms since 1970): it stands still while the page is hidden. */
  at(now: number): number {
    for (;;) {
      const seq = Atomics.load(this.#words, 0);
      if (seq % 2 !== 0) continue;
      const hidden = this.#floats[0]!;
      const hiddenAt = this.#floats[1]!;
      const visible = Atomics.load(this.#words, 1) !== 0;
      if (Atomics.load(this.#words, 0) === seq) return visible ? now - hidden : hiddenAt - hidden;
    }
  }

  /** The program starts at `now` (ms since 1970): frames and ticks count visible time from it.
   * The render worker calls it before anything reads `seconds`. */
  startAt(now: number): void {
    this.#floats[2] = this.at(now);
  }

  /** Seconds of visible time at `now` (ms since 1970) since the program started. */
  seconds(now: number): number {
    return (this.at(now) - this.#floats[2]!) / 1000;
  }
}

/** Seconds of visible running at `now` (ms since 1970), for each frame; null while the page is
 * hidden (run no frame). */
export const frameSeconds = (clock: VisibleClock, now: number) => (clock.visible ? clock.seconds(now) : null);

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
