// The main-thread shim: hands the canvas to the render worker, forwards its size (in device
// pixels) and the page's visibility, and shows fatal errors on the page and in the console.

import { errorMessage } from "./errors.ts";
import type { FromWorker, ToWorker } from "./messages.ts";
import { asksForTest, isLoopback, parseTestParams, putResult } from "./testmode.ts";

let shown = false;

/** Shows a fatal error over the canvas and logs it. */
function showFatal(text: string): void {
  console.error(`wrela: ${text}`);
  if (shown) return;
  shown = true;
  const box = document.createElement("pre");
  box.id = "wrela-error";
  box.setAttribute("role", "alert");
  box.textContent = `wrela stopped: ${text}`;
  document.body.append(box);
}

/** A failure before the worker could report it: show it, and in test mode (even with bad
 * parameters), end the test. */
function failEarly(e: unknown, testing: boolean): void {
  showFatal(errorMessage(e));
  if (testing) putResult(document.baseURI, "DONE", errorMessage(e)).catch((err: unknown) => console.error(errorMessage(err)));
}

/** The canvas's size in device pixels. */
function devicePixels(canvas: HTMLCanvasElement, entry?: ResizeObserverEntry): { width: number; height: number } {
  const box = entry?.devicePixelContentBoxSize?.[0];
  if (box) return { width: box.inlineSize, height: box.blockSize };
  const dpr = window.devicePixelRatio || 1;
  return { width: Math.round(canvas.clientWidth * dpr), height: Math.round(canvas.clientHeight * dpr) };
}

/** Calls `f` each time `window.devicePixelRatio` changes. */
function onPixelRatioChange(f: () => void): void {
  const query = matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
  query.addEventListener(
    "change",
    () => {
      f();
      onPixelRatioChange(f);
    },
    { once: true },
  );
}

function start(): void {
  const testing = isLoopback(location.hostname) && asksForTest(location.hash);
  try {
    const test = testing ? parseTestParams(location.hash) : null;
    const canvas = document.querySelector<HTMLCanvasElement>("canvas#screen");
    if (!canvas) throw new Error("the page has no <canvas id=\"screen\">");
    if (!("gpu" in navigator)) throw new Error("this browser doesn't support WebGPU");
    if (typeof canvas.transferControlToOffscreen !== "function") {
      throw new Error("this browser can't hand a canvas to a worker (no OffscreenCanvas)");
    }
    if (test) {
      // The canvas shows the test frame at its own size.
      const dpr = window.devicePixelRatio || 1;
      canvas.style.width = `${test.width / dpr}px`;
      canvas.style.height = `${test.height / dpr}px`;
    }
    const workerUrl = "worker.js";
    const worker = new Worker(new URL(workerUrl, import.meta.url), { type: "module", name: "wrela render" });
    worker.onmessage = (event: MessageEvent<FromWorker>) => showFatal(event.data.message);
    worker.onerror = (event) => {
      event.preventDefault();
      failEarly(new Error(`the render worker failed: ${event.message || "it couldn't start"}`), testing);
    };
    const offscreen = canvas.transferControlToOffscreen();
    const { width, height } = devicePixels(canvas);
    const msg: ToWorker = { type: "start", canvas: offscreen, base: document.baseURI, width, height, test };
    worker.postMessage(msg, [offscreen]);
    if (!test) {
      const resize = (entry?: ResizeObserverEntry) =>
        worker.postMessage({ type: "resize", ...devicePixels(canvas, entry) } satisfies ToWorker);
      const observer = new ResizeObserver(([entry]) => resize(entry));
      try {
        // The size in device pixels also changes with the pixel ratio alone (a move to
        // another screen, or zoom).
        observer.observe(canvas, { box: "device-pixel-content-box" });
      } catch {
        // Not supported (Safari): watch the pixel ratio.
        observer.observe(canvas);
        onPixelRatioChange(() => resize());
      }
      const visibility = () =>
        worker.postMessage({ type: "visibility", visible: document.visibilityState === "visible" } satisfies ToWorker);
      document.addEventListener("visibilitychange", visibility);
      visibility(); // the page may open hidden, in a background tab
    }
  } catch (e) {
    failEarly(e, testing);
  }
}

start();
