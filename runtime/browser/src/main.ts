// The main-thread shim: hands the canvas to the render worker, forwards its size (in device
// pixels) and the page's visibility, and shows fatal errors on the page and in the console.

import { AUDIO_QUANTUM, AUDIO_SAMPLE_RATE } from "./abi.gen.ts";
import { errorMessage } from "./errors.ts";
import { type FromWorker, type ToWorker, VOICE_PROCESSOR, type VoiceOptions } from "./messages.ts";
import { asksForTest, isLoopback, parseTestParams, putResult, type TestParams } from "./testmode.ts";

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

/** Plays the program's voice in an AudioWorklet (worklet.js), at the ABI's sample rate (the
 * browser resamples to the device's). In test mode it renders `test.audio` quanta offline
 * instead, saves the samples to `results/audio.f32`, and tells the worker. */
async function playVoice(voice: VoiceOptions, test: TestParams | null, worker: Worker): Promise<void> {
  const url = new URL("worklet.js", import.meta.url);
  const node = (ctx: BaseAudioContext) => {
    const n = new AudioWorkletNode(ctx, VOICE_PROCESSOR, {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [1],
      processorOptions: voice,
    });
    n.port.onmessage = (e: MessageEvent<{ message: string }>) => {
      const why = new Error(`the voice trapped: ${e.data.message}`);
      failEarly(why, test !== null);
    };
    n.connect(ctx.destination);
  };
  if (test) {
    if (test.audio === 0) return;
    const ctx = new OfflineAudioContext({ numberOfChannels: 1, length: test.audio * AUDIO_QUANTUM, sampleRate: AUDIO_SAMPLE_RATE });
    await ctx.audioWorklet.addModule(url);
    node(ctx);
    const samples = (await ctx.startRendering()).getChannelData(0);
    await putResult(document.baseURI, "audio.f32", samples.slice().buffer);
    worker.postMessage({ type: "audio-rendered" } satisfies ToWorker);
    return;
  }
  const ctx = new AudioContext({ sampleRate: AUDIO_SAMPLE_RATE, latencyHint: "interactive" });
  await ctx.audioWorklet.addModule(url);
  node(ctx);
  // A browser starts audio only after the person interacts with the page.
  if (ctx.state !== "running") {
    const resume = () => void ctx.resume();
    window.addEventListener("pointerdown", resume, { once: true });
    window.addEventListener("keydown", resume, { once: true });
  }
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
    worker.onmessage = (event: MessageEvent<FromWorker>) => {
      const msg = event.data;
      if (msg.type === "audio") {
        playVoice(msg.voice, test, worker).catch((e: unknown) => failEarly(e, testing));
      } else {
        showFatal(msg.message);
      }
    };
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
