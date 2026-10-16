import { BrowserPlatform } from "./platform.ts";
// The main-thread shim: hands the canvas to the render worker, forwards its size (in device
// pixels) and the page's visibility, and shows fatal errors on the page and in the console.

import { AUDIO_CHANNELS, AUDIO_QUANTUM, AUDIO_SAMPLE_RATE } from "./abi.gen.ts";
import { errorMessage, UnsupportedError } from "./errors.ts";
import { epochNow, InputRing } from "./input.ts";
import { Semantics } from "./semantics.ts";
import { listen, pollPads } from "./listen.ts";
import { type FromVoice, type FromWorker, type ToWorker, VOICE_PROCESSOR, type VoiceOptions } from "./messages.ts";
import { asksForTest, isLoopback, loaded, parseTestParams, putResult, sleepUntil, type TestParams } from "./testmode.ts";

let shown = false;

/** The page's own text (`<main id="wrela-text">`), if it has one: what the program shows, for
 * whoever doesn't run it, such as an agent that reads the page as HTML. The page's style keeps it
 * out of sight while the program loads; it is shown when the program can't run here, and removed
 * once the program draws, as the program's semantic tree says the same to a screen reader. */
function pageText(): HTMLElement | null {
  return document.querySelector<HTMLElement>("main#wrela-text");
}

/** Shows a fatal error over the canvas and logs it. When the machine can't run a program at all
 * (`unsupported`), it says plainly what a program needs. */
function showFatal(text: string, unsupported = false): void {
  stopVoice();
  console.error(`wrela: ${text}`);
  if (shown) return;
  shown = true;
  const box = document.createElement("pre");
  box.id = "wrela-error";
  box.setAttribute("role", "alert");
  if (unsupported) {
    box.className = "unsupported";
    box.textContent = `This needs WebGPU on a hardware GPU, which this browser can't give it (${text}).\nTry a current Chrome or Edge on a desktop or laptop computer.`;
    // A page with its own text shows it, under what's missing.
    const own = pageText();
    if (own) {
      document.documentElement.classList.add("text-shown");
      own.before(box);
      return;
    }
    // A page that is text as well (its Markdown alternate) says where that text is.
    const alternate = document.querySelector<HTMLLinkElement>('link[rel="alternate"][type="text/markdown"]');
    if (alternate && typeof alternate.href === "string" && alternate.href) {
      const a = document.createElement("a");
      a.href = alternate.href;
      a.textContent = "Read the same pages as plain text.";
      box.append("\n", a);
    }
  } else {
    box.textContent = `wrela stopped: ${text}`;
  }
  document.body.append(box);
}

/** A failure before the worker could report it: show it, and in test mode (even with bad
 * parameters), end the test. */
function failEarly(e: unknown, testing: boolean): void {
  showFatal(errorMessage(e), e instanceof UnsupportedError);
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

/** The page's audio context, made for the first voice, and the voice playing in it: a build
 * handed over to plays its voice in the same context (a browser starts a context only after the
 * person interacts with the page, so a new one would be silent). */
const audio: { context: Promise<AudioContext> | null; node: AudioWorkletNode | null; generation: number } = {
  context: null,
  node: null,
  generation: 0,
};

/** Disconnects the voice currently playing. */
function disconnectVoice(): void {
  if (!audio.node) return;
  audio.node.port.onmessage = null;
  audio.node.onprocessorerror = null;
  audio.node.disconnect();
  audio.node = null;
}

/** Stops the voice playing, including one still waiting for the worklet to load. */
function stopVoice(): void {
  audio.generation++;
  disconnectVoice();
}

/** Plays the program's voice in an AudioWorklet (worklet.js), at the ABI's sample rate (the
 * browser resamples to the device's). In test mode it renders `test.audio` quanta offline
 * instead, saves the samples to `results/audio.f32`, and tells the worker. */
async function playVoice(voice: VoiceOptions, test: TestParams | null, worker: Worker): Promise<void> {
  const generation = audio.generation;
  const url = new URL("worklet.js", import.meta.url);
  const node = (
    ctx: BaseAudioContext,
    quanta: number | null,
  ): { node: AudioWorkletNode; rendered: Promise<void>; failed: Promise<Error> } => {
    const rendered = quanta === null ? null : Promise.withResolvers<void>();
    const failed = Promise.withResolvers<Error>();
    const n = new AudioWorkletNode(ctx, VOICE_PROCESSOR, {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [AUDIO_CHANNELS],
      processorOptions: { ...voice, quanta },
    });
    let stopped = false;
    const fail = (message: string): void => {
      if (stopped) return;
      stopped = true;
      const why = new Error(`the voice trapped: ${message}`);
      n.port.onmessage = null;
      n.onprocessorerror = null;
      n.disconnect();
      if (audio.node === n) audio.node = null;
      showFatal(why.message);
      worker.postMessage({ type: "audio-failed", message } satisfies ToWorker);
      failed.resolve(why);
    };
    n.onprocessorerror = () => fail("the audio processor failed during initialization or rendering");
    n.port.onmessage = (e: MessageEvent<FromVoice>) => {
      const msg = e.data;
      if (msg.type === "print") {
        worker.postMessage(msg satisfies ToWorker);
      } else if (msg.type === "trap") {
        fail(msg.message);
      } else {
        rendered?.resolve();
      }
    };
    n.connect(ctx.destination);
    return { node: n, rendered: rendered?.promise ?? Promise.resolve(), failed: failed.promise };
  };
  if (test && test.live > 0) {
    // Real time: the voice plays for `live` seconds, then the browser's playback statistics
    // (Chrome's `AudioContext.playbackStats`, where it has them) say whether it kept up.
    const ctx = new AudioContext({ sampleRate: AUDIO_SAMPLE_RATE, latencyHint: "interactive" });
    await ctx.audioWorklet.addModule(url);
    const playing = node(ctx, null);
    await ctx.resume();
    const failure = await Promise.race([
      new Promise<null>((resolve) => setTimeout(() => resolve(null), test.live * 1000)),
      playing.failed,
    ]);
    if (failure !== null) {
      await ctx.close();
      return;
    }
    const stats = (ctx as unknown as { playbackStats?: Record<string, unknown> }).playbackStats;
    const reported = stats ? Object.fromEntries(Object.keys(Object.getPrototypeOf(stats)).concat(Object.keys(stats)).map((k) => [k, (stats as Record<string, unknown>)[k]]).filter(([, v]) => typeof v === "number")) : null;
    await putResult(document.baseURI, "live.json", JSON.stringify({ seconds: test.live, played: ctx.currentTime, state: ctx.state, baseLatency: ctx.baseLatency, outputLatency: ctx.outputLatency, playbackStats: reported }));
    await ctx.close();
    worker.postMessage({ type: "audio-rendered" } satisfies ToWorker);
    return;
  }
  if (test) {
    if (test.audio === 0) return;
    const ctx = new OfflineAudioContext({ numberOfChannels: AUDIO_CHANNELS, length: test.audio * AUDIO_QUANTUM, sampleRate: AUDIO_SAMPLE_RATE });
    await ctx.audioWorklet.addModule(url);
    const playing = node(ctx, test.audio);
    const done = Promise.race([playing.rendered.then(() => null), playing.failed]);
    const started = performance.now();
    const rendered = await ctx.startRendering();
    const ms = performance.now() - started;
    // The worklet's marker follows every print on its port, so all its lines reach the render
    // worker before `audio-rendered` lets the test save its log.
    const failure = await done;
    if (failure !== null) return;
    // The channels interleaved, as the native host gives them.
    const left = rendered.getChannelData(0);
    const right = rendered.getChannelData(1);
    const samples = new Float32Array(2 * left.length);
    for (let i = 0; i < left.length; i++) {
      samples[2 * i] = left[i]!;
      samples[2 * i + 1] = right[i]!;
    }
    await putResult(document.baseURI, "audio.f32", samples.buffer);
    // How long the render took, for the voice's speed (spike 14).
    await putResult(document.baseURI, "audio-ms.json", JSON.stringify({ ms }));
    worker.postMessage({ type: "audio-rendered" } satisfies ToWorker);
    return;
  }
  audio.context ??= (async () => {
    const ctx = new AudioContext({ sampleRate: AUDIO_SAMPLE_RATE, latencyHint: "interactive" });
    await ctx.audioWorklet.addModule(url);
    // A browser starts audio only after the person interacts with the page.
    if (ctx.state !== "running") {
      const resume = () => void ctx.resume();
      window.addEventListener("pointerdown", resume, { once: true });
      window.addEventListener("keydown", resume, { once: true });
    }
    return ctx;
  })();
  const ctx = await audio.context;
  if (generation !== audio.generation) return;
  disconnectVoice();
  audio.node = node(ctx, null).node;
}

/** Test mode, `latency`: `n` pointer moves sent through the DOM at random times over `ms`. */
async function sendLatencyEvents(canvas: HTMLCanvasElement, n: number, ms: number): Promise<void> {
  const r = canvas.getBoundingClientRect();
  const times = Array.from({ length: n }, () => Math.random() * ms).sort((a, b) => a - b);
  const start = performance.now();
  for (const t of times) {
    await sleepUntil(start + t);
    const x = r.left + Math.random() * r.width;
    const y = r.top + Math.random() * r.height;
    canvas.dispatchEvent(new PointerEvent("pointermove", { clientX: x, clientY: y, bubbles: true }));
  }
}

/** Test mode, `keylatency`: `n` presses of the right arrow key (down, then up 100 ms later)
 * sent through the DOM over `ms`, each in its own slot of at least 250 ms, at a random time in
 * it. */
async function sendKeyPresses(n: number, ms: number): Promise<void> {
  const slot = Math.max(ms / Math.max(n, 1), 250);
  const start = performance.now();
  const key = (type: string) => window.dispatchEvent(new KeyboardEvent(type, { code: "ArrowRight", key: "ArrowRight", bubbles: true }));
  for (let i = 0; i < n; i++) {
    const t = i * slot + Math.random() * (slot - 150);
    await sleepUntil(start + t);
    key("keydown");
    await sleepUntil(start + t + 100);
    key("keyup");
  }
}

/** Test mode, `clip`: the canvas recorded (MediaRecorder, VP9), from `clip-start` to
 * `clip-stop`, then saved as `results/clip.webm`. */
class Clip {
  private recorder: MediaRecorder | null = null;
  private readonly chunks: Blob[] = [];

  start(canvas: HTMLCanvasElement): void {
    const stream = canvas.captureStream(60);
    const mimeType = ["video/webm;codecs=vp9", "video/webm"].find((t) => MediaRecorder.isTypeSupported(t)) ?? "";
    this.recorder = new MediaRecorder(stream, { mimeType, videoBitsPerSecond: 16_000_000 });
    this.recorder.ondataavailable = (e) => this.chunks.push(e.data);
    this.recorder.start(1000);
  }

  /** Stops the recording and saves it (none if it never started). */
  async save(): Promise<void> {
    const r = this.recorder;
    if (r === null) throw new Error("the clip was never started (clipfrom past the test's frames?)");
    this.recorder = null;
    const stopped = new Promise<void>((resolve) => (r.onstop = () => resolve()));
    r.stop();
    await stopped;
    const bytes = await new Blob(this.chunks, { type: "video/webm" }).arrayBuffer();
    await putResult(document.baseURI, "clip.webm", bytes);
  }
}

function start(): void {
  const testing = isLoopback(location.hostname) && asksForTest(location.hash);
  try {
    const test = testing ? parseTestParams(location.hash) : null;
    const canvas = document.querySelector<HTMLCanvasElement>("canvas#screen");
    if (!canvas) throw new Error("the page has no <canvas id=\"screen\">");
    if (!("gpu" in navigator)) throw new UnsupportedError("this browser doesn't support WebGPU");
    if (typeof canvas.transferControlToOffscreen !== "function") {
      throw new UnsupportedError("this browser can't hand a canvas to a worker (no OffscreenCanvas)");
    }
    if (test) {
      // The canvas shows the test frame at its own size.
      const dpr = window.devicePixelRatio || 1;
      canvas.style.width = `${test.width / dpr}px`;
      canvas.style.height = `${test.height / dpr}px`;
    }
    const workerUrl = "worker.js";
    const worker = new Worker(new URL(workerUrl, import.meta.url), { type: "module", name: "wrela render" });
    const ring = InputRing.create();
    const platform = new BrowserPlatform(canvas, visible => worker.postMessage({ type: "visibility", visible: visible && document.visibilityState === "visible" } satisfies ToWorker));
    const clip = new Clip();
    worker.onmessage = (event: MessageEvent<FromWorker>) => {
      const msg = event.data;
      if (msg.type === "host-request") {
        platform.request(msg.service, msg.bytes).then(
          bytes => worker.postMessage({ type: "host-reply", id: msg.id, bytes, error: null } satisfies ToWorker),
          error => worker.postMessage({ type: "host-reply", id: msg.id, bytes: new Uint8Array(), error: errorMessage(error) } satisfies ToWorker),
        );
      } else if (msg.type === "presented") {
        pageText()?.remove();
        platform.presented();
      } else if (msg.type === "audio") {
        playVoice(msg.voice, test, worker).catch((e: unknown) => failEarly(e, testing));
      } else if (msg.type === "load-query") {
        worker.postMessage({ type: "load", opened_ms: performance.timeOrigin, resources: loaded(["navigation", "resource"]) } satisfies ToWorker);
      } else if (msg.type === "audio-stop") {
        stopVoice();
      } else if (msg.type === "reload") {
        location.reload();
      } else if (msg.type === "clip-start") {
        try {
          clip.start(canvas);
        } catch (e) {
          failEarly(e, testing);
        }
      } else if (msg.type === "clip-stop") {
        clip.save().then(
          () => worker.postMessage({ type: "clip-saved" } satisfies ToWorker),
          (e: unknown) => failEarly(e, testing),
        );
      } else if (msg.type === "latency-start") {
        const sent = test?.keylatency ? sendKeyPresses(test.keylatency, msg.ms) : sendLatencyEvents(canvas, test?.latency ?? 0, msg.ms);
        sent.then(
          () => worker.postMessage({ type: "latency-sent" } satisfies ToWorker),
          (e: unknown) => failEarly(e, testing),
        );
      } else {
        platform.failed();
        showFatal(msg.message, msg.unsupported);
      }
    };
    worker.onerror = (event) => {
      event.preventDefault();
      platform.failed();
      failEarly(new Error(`the render worker failed: ${event.message || "it couldn't start"}`), testing);
    };
    // The canvas keeps its size in device pixels for input positions; the worker owns its
    // drawing.
    const offscreen = canvas.transferControlToOffscreen();
    const { width, height } = devicePixels(canvas);
    const sized = { width, height };
    const overlay = new Semantics(canvas, (e) => ring.write(e, epochNow()), () => ({ rect: canvas.getBoundingClientRect(), ...sized }));
    platform.attach(listen(canvas, ring, epochNow, () => sized, overlay), overlay);
    // A test's input is its script: a pad on the machine that runs it isn't heard.
    if (!test) pollPads(ring, epochNow, () => platform.active && document.visibilityState === "visible" && document.hasFocus());
    // `wrela run` serves the page with this mark: it sends new literals and builds (hot reload).
    const mark = document.querySelector<HTMLMetaElement>('meta[name="wrela-live"]');
    const live = mark === null ? null : Number(mark.content ?? "0") || 0;
    const storage = document.querySelector<HTMLMetaElement>('meta[name="wrela-storage"]')?.content ?? null;
    const msg: ToWorker = { type: "start", canvas: offscreen, base: document.baseURI, storage, width, height, test, input: ring.buffer, live };
    worker.postMessage(msg, [offscreen]);
    if (!test) {
      const resize = (entry?: ResizeObserverEntry) => {
        Object.assign(sized, devicePixels(canvas, entry));
        worker.postMessage({ type: "resize", ...sized } satisfies ToWorker);
      };
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
        worker.postMessage({ type: "visibility", visible: platform.active && document.visibilityState === "visible" } satisfies ToWorker);
      document.addEventListener("visibilitychange", visibility);
      visibility(); // the page may open hidden, in a background tab
    }
  } catch (e) {
    failEarly(e, testing);
  }
}

start();
