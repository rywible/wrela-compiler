// Messages between the main thread (main.ts) and the render worker (worker.ts).

import type { TestParams } from "./testmode.ts";

export type ToWorker =
  | {
      type: "start";
      canvas: OffscreenCanvas;
      /** The page's URL: the build's files and `results/` resolve against it. */
      base: string;
      /** The canvas's size in device pixels. */
      width: number;
      height: number;
      /** Test mode's parameters, or null to run normally. */
      test: TestParams | null;
    }
  | { type: "resize"; width: number; height: number }
  | { type: "visibility"; visible: boolean };

export type FromWorker = { type: "fatal"; message: string };
