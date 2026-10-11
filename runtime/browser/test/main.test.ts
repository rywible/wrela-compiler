// The main-thread shim (src/main.ts) on a fake page: each test loads a fresh copy of it.

import { afterEach, expect, test } from "bun:test";
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
  worker: { onmessage: ((event: MessageEvent<FromWorker>) => void) | null } | null;
}

let loads = 0;

/** Loads main.ts on a page at `url`, hidden or not, whose ResizeObserver may lack
 * `device-pixel-content-box` (as Safari's does). */
async function load(url: string, options: { hidden?: boolean; devicePixelBox?: boolean } = {}): Promise<Page> {
  const page: Page = { posted: [], puts: [], shown: [], observed: [], mediaListeners: [], visibilityListeners: [], worker: null };
  const canvas = {
    style: {},
    clientWidth: 100,
    clientHeight: 50,
    transferControlToOffscreen: () => ({}),
    addEventListener: () => {},
  };
  const u = new URL(url);
  fake("location", { hostname: u.hostname, hash: u.hash });
  fake("window", { devicePixelRatio: 2, addEventListener: () => {} });
  fake("document", {
    baseURI: `${u.origin}${u.pathname}`,
    visibilityState: options.hidden ? "hidden" : "visible",
    querySelector: () => canvas,
    createElement: () => ({ setAttribute() {}, textContent: "" }),
    body: { append: (el: { textContent: string }) => page.shown.push(el.textContent) },
    addEventListener: (type: string, f: () => void) => type === "visibilitychange" && page.visibilityListeners.push(f),
  });
  fake("gpu", {}, navigator);
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
  expect(page.shown).toEqual(["wrela stopped: unknown test parameter `frame` (expected frames, width, height, fps, workers, audio, live, timestamps, nohash, ticklog, inflight, snap, snapfrom, input, latency, keylatency, paced, tickdelay, framedelay, salt, saturate, clip or clipfrom)"]);
  expect(page.puts).toEqual(["PUT http://127.0.0.1:8000/page/results/DONE"]);
  expect(page.posted).toEqual([]);
});

test("a public page ignores #test", async () => {
  const page = await load("https://example.com/game/#test&frame=3");
  expect(page.shown).toEqual([]);
  expect(page.puts).toEqual([]);
  expect(page.posted.map((m) => m.type)).toEqual(["start", "visibility"]);
});

test("the worker learns the page's visibility at once, and on each change", async () => {
  const page = await load("https://example.com/game/", { hidden: true });
  expect(page.posted.at(-1)).toEqual({ type: "visibility", visible: false });
  (document as { visibilityState: string }).visibilityState = "visible";
  for (const f of page.visibilityListeners) f();
  expect(page.posted.at(-1)).toEqual({ type: "visibility", visible: true });
});

test("the canvas is watched in device pixels, so a pixel ratio change resizes it", async () => {
  const page = await load("https://example.com/game/");
  expect(page.observed).toEqual([{ box: "device-pixel-content-box" }]);
  expect(page.mediaListeners).toEqual([]);
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
    },
  );
  const page = await load("http://127.0.0.1/game/#test&audio=1");
  page.worker!.onmessage!(new MessageEvent("message", { data: { type: "audio", voice: {} as VoiceOptions } satisfies FromWorker }));
  await Bun.sleep(5);
  expect(page.posted.slice(-2)).toEqual([{ type: "print", line: "offline voice" }, { type: "audio-rendered" }]);
});
