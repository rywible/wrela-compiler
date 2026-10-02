// The main-thread shim: hands the canvas to the render worker, forwards its size (in device
// pixels) and the page's visibility, and shows fatal errors on the page and in the console.

import type { FromWorker, ToWorker } from "./messages.ts";
import { isLoopback, parseTestParams, putResult, type TestParams } from "./testmode.ts";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

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

/** A failure before the worker could report it: show it, and in test mode, end the test. */
function failEarly(e: unknown, test: TestParams | null): void {
  showFatal(message(e));
  if (test) putResult(document.baseURI, "DONE", message(e)).catch((err: unknown) => console.error(message(err)));
}

/** The canvas's size in device pixels. */
function devicePixels(canvas: HTMLCanvasElement, entry?: ResizeObserverEntry): { width: number; height: number } {
  const box = entry?.devicePixelContentBoxSize?.[0];
  if (box) return { width: box.inlineSize, height: box.blockSize };
  const dpr = window.devicePixelRatio || 1;
  return { width: Math.round(canvas.clientWidth * dpr), height: Math.round(canvas.clientHeight * dpr) };
}

function start(): void {
  let test: TestParams | null = null;
  try {
    test = isLoopback(location.hostname) ? parseTestParams(location.hash) : null;
    const canvas = document.querySelector<HTMLCanvasElement>("canvas#screen");
    if (!canvas) throw new Error("the page has no <canvas id=\"screen\">");
    if (!("gpu" in navigator)) throw new Error("this browser doesn't support WebGPU");
    if (typeof canvas.transferControlToOffscreen !== "function") {
      throw new Error("this browser can't hand a canvas to a worker (no OffscreenCanvas)");
    }
    if (test) {
      // The canvas shows the test frame at its own size.
      canvas.style.width = `${test.width / (window.devicePixelRatio || 1)}px`;
      canvas.style.height = `${test.height / (window.devicePixelRatio || 1)}px`;
    }
    const workerUrl = "worker.js";
    const worker = new Worker(new URL(workerUrl, import.meta.url), { type: "module", name: "wrela render" });
    worker.onmessage = (event: MessageEvent<FromWorker>) => showFatal(event.data.message);
    worker.onerror = (event) => {
      event.preventDefault();
      failEarly(new Error(`the render worker failed: ${event.message || "it couldn't start"}`), test);
    };
    const offscreen = canvas.transferControlToOffscreen();
    const { width, height } = devicePixels(canvas);
    const msg: ToWorker = { type: "start", canvas: offscreen, base: document.baseURI, width, height, test };
    worker.postMessage(msg, [offscreen]);
    if (!test) {
      new ResizeObserver(([entry]) => {
        worker.postMessage({ type: "resize", ...devicePixels(canvas, entry) } satisfies ToWorker);
      }).observe(canvas);
      document.addEventListener("visibilitychange", () => {
        worker.postMessage({ type: "visibility", visible: document.visibilityState === "visible" } satisfies ToWorker);
      });
    }
  } catch (e) {
    failEarly(e, test);
  }
}

start();
