// The main-thread shim (src/main.ts) on a fake page: each test loads a fresh copy of it.

import { afterEach, expect, test } from "bun:test";
import { InputRing, padButtonEvent } from "../src/input.ts";
import type { FromVoice, FromWorker, ToWorker, VoiceOptions } from "../src/messages.ts";

const saved = new Map<string, PropertyDescriptor | undefined>();

/** Sets a global for one test. */
function fake(name: string, value: unknown, on: object = globalThis): void {
  const key = on === globalThis ? name : `navigator.${name}`;
  if (!saved.has(key)) saved.set(key, Object.getOwnPropertyDescriptor(on, name));
  Object.defineProperty(on, name, { value, configurable: true, writable: true });
}

afterEach(() => {
  for (const [key, descriptor] of saved) {
    const [on, name] = key.startsWith("navigator.") ? [navigator, key.slice(10)] : [globalThis, key];
    if (descriptor) Object.defineProperty(on, name, descriptor);
    else delete (on as Record<string, unknown>)[name];
  }
  saved.clear();
});

interface Page {
  posted: ToWorker[];
  puts: string[];
  shown: string[];
  observed: (ResizeObserverOptions | undefined)[];
  /** Fires the page's media query listeners (a pixel ratio change). */
  mediaListeners: (() => void)[];
  visibilityListeners: (() => void)[];
  canvasEvents: Map<string, (event: unknown) => void>;
  worker: { onmessage: ((event: MessageEvent<FromWorker>) => void) | null } | null;
  /** The page's own text, when a test gives it one: what happened to it. */
  text: string[];
  classes: string[];
}

let loads = 0;

/** Loads main.ts on a page at `url`, hidden or not, whose ResizeObserver may lack
 * `device-pixel-content-box` (as Safari's does). */
async function load(url: string, options: { hidden?: boolean; devicePixelBox?: boolean; focused?: boolean; gpu?: boolean; alternate?: string; links?: { href: string; text: string }[]; text?: boolean; preload?: string[] } = {}): Promise<Page> {
  const page: Page = { posted: [], puts: [], shown: [], observed: [], mediaListeners: [], visibilityListeners: [], canvasEvents: new Map(), worker: null, text: [], classes: [] };
  // The page's own text records a box put before it, and its removal.
  const own = options.text ? {
    before: (el: { textContent: string; className?: string }) => page.text.push(`before: ${el.className ? `[${el.className}] ` : ""}${el.textContent.split("\n")[0]}`),
    remove: () => page.text.push("removed"),
  } : null;
  const canvas = {
    style: {},
    clientWidth: 100,
    clientHeight: 50,
    transferControlToOffscreen: () => ({}),
    addEventListener: (name: string, listener: (event: unknown) => void) => page.canvasEvents.set(name, listener),
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 100, height: 50 }),
  };
  const u = new URL(url);
  fake("location", { hostname: u.hostname, hash: u.hash });
  fake("window", { devicePixelRatio: 2, addEventListener: () => {} });
  fake("document", {
    baseURI: `${u.origin}${u.pathname}`,
    visibilityState: options.hidden ? "hidden" : "visible",
    hasFocus: () => options.focused ?? true,
    // The canvas, or the page's Markdown alternate when a test gives one.
    querySelector: (selector: string) => selector.startsWith("link") ? (options.alternate ? { href: options.alternate } : null) : selector.startsWith("main") ? own : canvas,
    // The build's files the page preloads.
    querySelectorAll: (selector: string) => selector.startsWith("link[rel=\"preload\"]") ? (options.preload ?? []).map((href) => ({ href })) : [],
    documentElement: { classList: { add: (name: string) => page.classes.push(name) } },
    createElement: (tag: string) => tag === "a"
      ? { href: "", textContent: "" }
      : { setAttribute() {}, textContent: "", append(_: string, a: { href: string; textContent: string }) { options.links?.push({ href: a.href, text: a.textContent }); } },
    // A box shown with a class (`unsupported`) is recorded with it.
    body: { append: (el: { textContent: string; className?: string }) => page.shown.push(el.className ? `[${el.className}] ${el.textContent}` : el.textContent) },
    addEventListener: (type: string, f: () => void) => type === "visibilitychange" && page.visibilityListeners.push(f),
  });
  fake("gpu", {}, navigator);
  // A browser without WebGPU has no `navigator.gpu` at all (afterEach restores it).
  if (options.gpu === false) delete (navigator as unknown as Record<string, unknown>).gpu;
  fake("fetch", async (url: URL, init?: RequestInit) => {
    page.puts.push(`${init?.method} ${url}`);
    return new Response("");
  });
  fake("matchMedia", () => ({
    addEventListener: (_: string, f: () => void) => page.mediaListeners.push(f),
  }));
  fake(
    "Worker",
    class {
      onmessage: ((event: MessageEvent<FromWorker>) => void) | null = null;
      onerror = null;
      constructor() {
        page.worker = this;
      }
      postMessage(m: ToWorker) {
        page.posted.push(m);
      }
    },
  );
  fake(
    "ResizeObserver",
    class {
      observe(_: unknown, opts?: ResizeObserverOptions) {
        if (opts?.box === "device-pixel-content-box" && options.devicePixelBox === false) {
          throw new TypeError("unsupported box");
        }
        page.observed.push(opts);
      }
    },
  );
  fake("console", { ...console, error: () => {} });
  await import(`../src/main.ts?load=${loads++}`);
  await Bun.sleep(1);
  return page;
}

test("a #test fragment with bad parameters still ends the test", async () => {
  const page = await load("http://127.0.0.1:8000/page/#test&frame=3");
  expect(page.shown).toEqual(["wrela stopped: unknown test parameter `frame` (expected frames, width, height, fps, workers, audio, live, timestamps, nohash, ticklog, inflight, snap, snapfrom, input, features, latency, keylatency, paced, tickdelay, framedelay, salt, saturate, clip or clipfrom)"]);
  expect(page.puts).toEqual(["PUT http://127.0.0.1:8000/page/results/DONE"]);
  expect(page.posted).toEqual([]);
});

test("a public page ignores #test", async () => {
  const page = await load("https://example.com/game/#test&frame=3");
  expect(page.shown).toEqual([]);
  expect(page.puts).toEqual([]);
  expect(page.posted.map((m) => m.type)).toEqual(["start", "visibility"]);
});

test("the page names the build's files it preloads to the worker, and fetches them for it", async () => {
  const page = await load("https://example.com/game/", { preload: ["https://example.com/game/manifest.json", "https://example.com/game/game.wasm"] });
  const start = page.posted.find((msg) => msg.type === "start");
  expect(start?.type === "start" && start.given).toEqual(["https://example.com/game/manifest.json", "https://example.com/game/game.wasm"]);
  expect(page.puts).toEqual(["undefined https://example.com/game/manifest.json", "undefined https://example.com/game/game.wasm"]);
});

test("a browser without WebGPU is told what a program needs, not that it failed", async () => {
  const page = await load("https://example.com/game/", { gpu: false });
  expect(page.shown).toEqual(["[unsupported] This needs WebGPU on a hardware GPU, which this browser can't give it (this browser doesn't support WebGPU).\nA current Chrome, Edge or Safari has it, on a computer or a phone."]);
  expect(page.posted).toEqual([]);
});

test("a page that is text as well links to its text when it can't run", async () => {
  const links: { href: string; text: string }[] = [];
  const page = await load("https://docs.example/", { gpu: false, alternate: "https://docs.example/llms.txt", links });
  expect(page.shown[0]).toStartWith("[unsupported] This needs WebGPU");
  expect(links).toEqual([{ href: "https://docs.example/llms.txt", text: "Read the same pages as plain text." }]);
});

test("a page with its own text shows it when it can't run, and drops it once the program draws", async () => {
  const cannot = await load("https://docs.example/", { gpu: false, alternate: "https://docs.example/llms.txt", text: true });
  expect(cannot.text).toEqual(["before: [unsupported] This needs WebGPU on a hardware GPU, which this browser can't give it (this browser doesn't support WebGPU)."]);
  expect(cannot.classes).toEqual(["text-shown"]);
  expect(cannot.shown).toEqual([]);
  const runs = await load("https://docs.example/", { text: true });
  expect(runs.text).toEqual([]);
  runs.worker!.onmessage!(new MessageEvent("message", { data: { type: "presented" } satisfies FromWorker }));
  expect(runs.text).toEqual(["removed"]);
  expect(runs.classes).toEqual([]);
});

test("a page that can't share memory with its worker is a browser that can't run it", async () => {
  const saved = Object.getOwnPropertyDescriptor(globalThis, "SharedArrayBuffer");
  Reflect.deleteProperty(globalThis, "SharedArrayBuffer");
  try {
    const page = await load("https://docs.example/", { text: true });
    expect(page.text).toEqual(["before: [unsupported] This needs WebGPU on a hardware GPU, which this browser can't give it (this page can't share memory with its worker here)."]);
    expect(page.posted).toEqual([]);
  } finally {
    if (saved) Object.defineProperty(globalThis, "SharedArrayBuffer", saved);
  }
});

test("a program that fails before it draws leaves its page's own text to read", async () => {
  const page = await load("https://docs.example/", { text: true });
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "fatal", message: "device lost", unsupported: false } satisfies FromWorker }));
  expect(page.text).toEqual(["before: wrela stopped: device lost"]);
  expect(page.classes).toEqual(["text-shown"]);
});

test("a machine without a hardware adapter is told the same; other failures still say they stopped", async () => {
  const unsupported = await load("https://example.com/game/");
  unsupported.worker!.onmessage!(new MessageEvent("message", { data: { type: "fatal", message: "WebGPU isn't available here (no adapter)", unsupported: true } satisfies FromWorker }));
  expect(unsupported.shown).toEqual(["[unsupported] This needs WebGPU on a hardware GPU, which this browser can't give it (WebGPU isn't available here (no adapter)).\nA current Chrome, Edge or Safari has it, on a computer or a phone."]);
  const failed = await load("https://example.com/game/");
  failed.worker!.onmessage!(new MessageEvent("message", { data: { type: "fatal", message: "device lost", unsupported: false } satisfies FromWorker }));
  expect(failed.shown).toEqual(["wrela stopped: device lost"]);
});

test("the worker learns the page's visibility at once, and on each change", async () => {
  const page = await load("https://example.com/game/", { hidden: true });
  expect(page.posted.at(-1)).toEqual({ type: "visibility", visible: false });
  (document as { visibilityState: string }).visibilityState = "visible";
  for (const f of page.visibilityListeners) f();
  expect(page.posted.at(-1)).toEqual({ type: "visibility", visible: true });
});

test("an unfocused embedded document defers gamepad input until it gains focus", async () => {
  const pending: FrameRequestCallback[] = [];
  fake("requestAnimationFrame", (callback: FrameRequestCallback) => pending.push(callback));
  fake("getGamepads", () => [{ connected: true, mapping: "standard", buttons: [{ value: 1, pressed: true }], axes: [] }], navigator);
  const options = { focused: false };
  const page = await load("https://example.com/game/", options);
  const start = page.posted.find(msg => msg.type === "start")!;
  const ring = new InputRing(start.input);
  pending.shift()!(0);
  expect(ring.read().events.length).toBe(0);
  options.focused = true;
  (document as { visibilityState: string }).visibilityState = "hidden";
  pending.shift()!(0);
  expect(ring.read().events.length).toBe(0);
  (document as { visibilityState: string }).visibilityState = "visible";
  pending.shift()!(0);
  expect(ring.read().events).toEqual(padButtonEvent(0, 0, 1, true));
});

test("the canvas is watched in device pixels, so a pixel ratio change resizes it", async () => {
  const page = await load("https://example.com/game/");
  expect(page.observed).toEqual([{ box: "device-pixel-content-box" }]);
  expect(page.mediaListeners).toEqual([]);
});

test("input uses the fitted frame size when the canvas exceeds the GPU limit", async () => {
  const page = await load("https://example.com/game/", { devicePixelBox: false });
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "screen-limit", max: 100 } satisfies FromWorker }));
  const start = page.posted.find(msg => msg.type === "start")!;
  const ring = new InputRing(start.input);
  page.canvasEvents.get("pointermove")!({ pointerId: 1, clientX: 50, clientY: 25 });
  const event = ring.read().events;
  const view = new DataView(event.buffer, event.byteOffset, event.byteLength);
  expect([view.getFloat32(8, true), view.getFloat32(12, true)]).toEqual([50, 25]);
  expect(page.shown).toEqual([]);
  // Resizes use the same limit; smaller canvases retain their full device-pixel size.
  (window as { devicePixelRatio: number }).devicePixelRatio = 3;
  page.mediaListeners.at(-1)!();
  expect(page.posted.at(-1)).toEqual({ type: "resize", width: 100, height: 50 });
  (window as { devicePixelRatio: number }).devicePixelRatio = 0.5;
  page.mediaListeners.at(-1)!();
  expect(page.posted.at(-1)).toEqual({ type: "resize", width: 50, height: 25 });
});

test("without device-pixel boxes, a pixel ratio change still resizes the canvas", async () => {
  const page = await load("https://example.com/game/", { devicePixelBox: false });
  expect(page.observed).toEqual([undefined]);
  (window as { devicePixelRatio: number }).devicePixelRatio = 3;
  page.mediaListeners[0]!();
  expect(page.posted.at(-1)).toEqual({ type: "resize", width: 300, height: 150 });
  // It keeps watching at the new ratio.
  expect(page.mediaListeners.length).toBe(2);
});

test("audio print messages are relayed to the render worker and traps still fail the page", async () => {
  let voicePort: { onmessage: ((event: MessageEvent<FromVoice>) => void) | null } | null = null;
  fake(
    "AudioContext",
    class {
      readonly audioWorklet = { addModule: async () => {} };
      readonly destination = {};
      readonly state = "running";
    },
  );
  fake(
    "AudioWorkletNode",
    class {
      readonly port = { onmessage: null as ((event: MessageEvent<FromVoice>) => void) | null };
      constructor() {
        voicePort = this.port;
      }
      connect() {}
      disconnect() {}
    },
  );
  const page = await load("https://example.com/game/");
  const voice = {} as VoiceOptions;
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice } satisfies FromWorker }));
  await Bun.sleep(1);
  voicePort!.onmessage!(new MessageEvent("message", { data: { type: "print", line: "from voice" } satisfies FromVoice }));
  expect(page.posted.at(-1)).toEqual({ type: "print", line: "from voice" });

  voicePort!.onmessage!(new MessageEvent("message", { data: { type: "trap", message: "boom" } satisfies FromVoice }));
  expect(page.shown).toEqual(["wrela stopped: the voice trapped: boom"]);
  expect(page.posted.at(-1)).toEqual({ type: "audio-failed", message: "boom" });
});

test("stopping a voice while its worklet loads keeps it from starting later", async () => {
  const moduleReady = Promise.withResolvers<void>();
  let nodes = 0;
  fake(
    "AudioContext",
    class {
      readonly audioWorklet = { addModule: () => moduleReady.promise };
      readonly destination = {};
      readonly state = "running";
    },
  );
  fake(
    "AudioWorkletNode",
    class {
      readonly port = { onmessage: null };
      onprocessorerror = null;
      constructor() {
        nodes++;
      }
      connect() {}
      disconnect() {}
    },
  );
  const page = await load("https://example.com/game/");
  const voice = {} as VoiceOptions;
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice } satisfies FromWorker }));
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio-stop" } satisfies FromWorker }));
  moduleReady.resolve();
  await Bun.sleep(1);
  expect(nodes).toBe(0);
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice } satisfies FromWorker }));
  await Bun.sleep(1);
  expect(nodes).toBe(1);
});

test("a fatal render worker error stops the voice already playing", async () => {
  let connected = false;
  fake("AudioContext", class {
    readonly audioWorklet = { addModule: async () => {} };
    readonly destination = {};
    readonly state = "running";
  });
  fake("AudioWorkletNode", class {
    readonly port = { onmessage: null };
    onprocessorerror = null;
    connect() { connected = true; }
    disconnect() { connected = false; }
  });
  const page = await load("https://example.com/game/");
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice: {} as VoiceOptions } satisfies FromWorker }));
  await Bun.sleep(1);
  expect(connected).toBe(true);
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "fatal", message: "frame failed", unsupported: false } satisfies FromWorker }));
  expect(connected).toBe(false);
  expect(page.shown).toEqual(["wrela stopped: frame failed"]);
});

test("a fatal render worker error keeps a loading voice from starting later", async () => {
  const loaded = Promise.withResolvers<void>();
  let nodes = 0;
  fake("AudioContext", class {
    readonly audioWorklet = { addModule: () => loaded.promise };
    readonly destination = {};
    readonly state = "running";
  });
  fake("AudioWorkletNode", class {
    readonly port = { onmessage: null };
    onprocessorerror = null;
    constructor() { nodes++; }
    connect() {}
    disconnect() {}
  });
  const page = await load("https://example.com/game/");
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice: {} as VoiceOptions } satisfies FromWorker }));
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "fatal", message: "frame failed", unsupported: false } satisfies FromWorker }));
  loaded.resolve();
  await Bun.sleep(1);
  expect(nodes).toBe(0);
});

test("stopping test audio while its worklet loads keeps it from starting later", async () => {
  for (const mode of ["audio=1", "live=1"]) {
    const loaded = Promise.withResolvers<void>();
    let nodes = 0;
    const Context = class {
      readonly audioWorklet = { addModule: () => loaded.promise };
      readonly destination = {};
      readonly state = "running";
      async resume() {}
      async close() {}
      async startRendering() { return { getChannelData: () => new Float32Array(128) }; }
    };
    fake("AudioContext", Context);
    fake("OfflineAudioContext", Context);
    fake("AudioWorkletNode", class {
      readonly port = { onmessage: null };
      onprocessorerror = null;
      constructor() { nodes++; }
      connect() {}
      disconnect() {}
    });
    const page = await load(`http://127.0.0.1/game/#test&${mode}`);
    page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice: {} as VoiceOptions } satisfies FromWorker }));
    page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio-stop" } satisfies FromWorker }));
    loaded.resolve();
    await Bun.sleep(5);
    expect(nodes).toBe(0);
    expect(page.posted.some((message) => message.type === "audio-rendered")).toBe(false);
  }
});

test("a fatal worker error stops live test audio and prevents a successful audio result", async () => {
  let connected = false;
  fake("AudioContext", class {
    readonly audioWorklet = { addModule: async () => {} };
    readonly destination = {};
    readonly state = "running";
    async resume() {}
    async close() {}
  });
  fake("AudioWorkletNode", class {
    readonly port = { onmessage: null };
    onprocessorerror = null;
    connect() { connected = true; }
    disconnect() { connected = false; }
  });
  const page = await load("http://127.0.0.1/game/#test&live=1");
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice: {} as VoiceOptions } satisfies FromWorker }));
  await Bun.sleep(1);
  expect(connected).toBe(true);
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "fatal", message: "frame failed", unsupported: false } satisfies FromWorker }));
  expect(connected).toBe(false);
  await Bun.sleep(1050);
  expect(page.posted.some((message) => message.type === "audio-rendered")).toBe(false);
});

test("offline audio waits for the worklet's print marker before reporting it rendered", async () => {
  let voicePort: { onmessage: ((event: MessageEvent<FromVoice>) => void) | null } | null = null;
  fake(
    "OfflineAudioContext",
    class {
      readonly audioWorklet = { addModule: async () => {} };
      readonly destination = {};
      async startRendering() {
        voicePort!.onmessage!(new MessageEvent("message", { data: { type: "print", line: "offline voice" } satisfies FromVoice }));
        voicePort!.onmessage!(new MessageEvent("message", { data: { type: "rendered" } satisfies FromVoice }));
        return { getChannelData: () => new Float32Array(128) };
      }
    },
  );
  fake(
    "AudioWorkletNode",
    class {
      readonly port = { onmessage: null as ((event: MessageEvent<FromVoice>) => void) | null };
      constructor() {
        voicePort = this.port;
      }
      connect() {}
      disconnect() {}
    },
  );
  const page = await load("http://127.0.0.1/game/#test&audio=1");
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice: {} as VoiceOptions } satisfies FromWorker }));
  await Bun.sleep(5);
  expect(page.posted.slice(-2)).toEqual([{ type: "print", line: "offline voice" }, { type: "audio-rendered" }]);
});

test("an offline audio trap fails the render worker without waiting for a rendered marker", async () => {
  let voicePort: { onmessage: ((event: MessageEvent<FromVoice>) => void) | null } | null = null;
  fake(
    "OfflineAudioContext",
    class {
      readonly audioWorklet = { addModule: async () => {} };
      readonly destination = {};
      async startRendering() {
        voicePort!.onmessage!(new MessageEvent("message", { data: { type: "trap", message: "offline boom" } satisfies FromVoice }));
        return { getChannelData: () => new Float32Array(128) };
      }
    },
  );
  fake(
    "AudioWorkletNode",
    class {
      readonly port = { onmessage: null as ((event: MessageEvent<FromVoice>) => void) | null };
      constructor() {
        voicePort = this.port;
      }
      connect() {}
      disconnect() {}
    },
  );
  const page = await load("http://127.0.0.1/game/#test&audio=1");
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice: {} as VoiceOptions } satisfies FromWorker }));
  await Bun.sleep(5);
  expect(page.shown).toEqual(["wrela stopped: the voice trapped: offline boom"]);
  expect(page.posted.at(-1)).toEqual({ type: "audio-failed", message: "offline boom" });
  expect(page.posted.some((message) => message.type === "audio-rendered")).toBe(false);
});

test("an audio processor construction error fails an offline render instead of leaving it pending", async () => {
  let processorError: ((event: Event) => void) | null = null;
  fake(
    "OfflineAudioContext",
    class {
      readonly audioWorklet = { addModule: async () => {} };
      readonly destination = {};
      async startRendering() {
        processorError!(new Event("processorerror"));
        return { getChannelData: () => new Float32Array(128) };
      }
    },
  );
  fake(
    "AudioWorkletNode",
    class {
      readonly port = { onmessage: null as ((event: MessageEvent<FromVoice>) => void) | null };
      onprocessorerror: ((event: Event) => void) | null = null;
      constructor() {
        processorError = (event) => this.onprocessorerror?.(event);
      }
      connect() {}
      disconnect() {}
    },
  );
  const page = await load("http://127.0.0.1/game/#test&audio=1");
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice: {} as VoiceOptions } satisfies FromWorker }));
  await Bun.sleep(5);
  const message = "the audio processor failed during initialization or rendering";
  expect(page.shown).toEqual([`wrela stopped: the voice trapped: ${message}`]);
  expect(page.posted.at(-1)).toEqual({ type: "audio-failed", message });
  expect(page.posted.some((posted) => posted.type === "audio-rendered")).toBe(false);
});
