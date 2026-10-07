// Hot reload (`wrela run`, compiler/driver/src/live.rs): the server that serves the page watches
// the program's files. A change that touches only lifted literals comes as their new values,
// which the running program takes between two frames (`__lift_set`); any other change comes as a
// new build, which replaces the program in place, in this page (worker.ts's `swap`). The render
// worker asks for what's new with a long poll, and tells the server the first frame that showed
// each change (`/live/shown`), which `wrela run` prints and its tests time.

import type { Program } from "./program.ts";

/** What the server says has changed, numbered in order. */
export type LiveEvent =
  | { seq: number; kind: "literals"; values: [number, number][] }
  | { seq: number; kind: "build"; base: string }
  | { seq: number; kind: "error"; text: string };

/** Waits `ms` milliseconds. */
const pause = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

export class Live {
  /** Changes that have come and aren't applied yet, oldest first. */
  readonly #events: LiveEvent[] = [];
  /** The newest change the server has sent. */
  #seq = 0;
  /** The newest change applied that no frame has shown yet. */
  #unshown: number | null = null;

  /** `base`: the page's URL, which the server's paths resolve against. */
  constructor(readonly base: string) {}

  /** Asks the server for what's new, again and again, until the page goes away. */
  start(): void {
    void this.#poll();
  }

  async #poll(): Promise<void> {
    for (;;) {
      try {
        const response = await fetch(new URL(`/live/next?after=${this.#seq}`, this.base), { cache: "no-store" });
        if (!response.ok) throw new Error(`${response.status}`);
        for (const e of (await response.json()) as LiveEvent[]) {
          if (e.seq <= this.#seq) continue;
          this.#events.push(e);
          this.#seq = e.seq;
        }
      } catch {
        // The server is gone or restarting: ask again soon.
        await pause(250);
      }
    }
  }

  /** Applies the changes that have come to `program`, up to a new build: that build's URL, if
   * one is next (it stays next until `swapped`). */
  take(program: Program): string | null {
    while (this.#events.length > 0) {
      const e = this.#events[0]!;
      if (e.kind === "build") return new URL(e.base, this.base).href;
      this.#events.shift();
      if (e.kind === "literals") {
        for (const [i, v] of e.values) program.setLiteral(i, v);
        this.#unshown = e.seq;
      } else {
        console.error(`wrela run: the files don't build:\n${e.text}`);
      }
    }
    return null;
  }

  /** The new build `take` gave is in place. */
  swapped(): void {
    const e = this.#events.shift();
    if (e) this.#unshown = e.seq;
  }

  /** Frame `frame` is drawn: the server learns the change it's the first to show. */
  drawn(frame: number): void {
    if (this.#unshown === null) return;
    const body = JSON.stringify({ seq: this.#unshown, frame });
    this.#unshown = null;
    void fetch(new URL("/live/shown", this.base), { method: "POST", body }).catch(() => {});
  }
}
