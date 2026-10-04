// Test mode: `index.html#test&frames=60&width=1920&height=1080&fps=60&workers=4&audio=750` runs a
// fixed number of frames at fixed times and canvas size, its parallel jobs on a fixed number of
// threads (the program's own included), then saves the last frame and the state hash to
// `results/` for tools/headless.py. With `audio`, the main thread also renders that many
// quanta of the program's voice offline, in an AudioWorklet, and saves the samples; with
// `timestamps=1`, each pass's GPU time (`timings.json`, where the device has timestamp queries). It's part of the shipped bundle, so the agreement test runs
// the exact bytes a game ships, but only a page served from this machine (tools/serve.py and
// tools/headless.py bind 127.0.0.1) enters it: a game's public URL ignores `#test`.

export interface TestParams {
  frames: number;
  width: number;
  height: number;
  fps: number;
  workers: number;
  /** Quanta of the voice to render (0: none). */
  audio: number;
  /** 1: time each pass on the GPU. */
  timestamps: number;
}

export const TEST_DEFAULTS: TestParams = { frames: 60, width: 1920, height: 1080, fps: 60, workers: 1, audio: 0, timestamps: 0 };
const KEYS = ["frames", "width", "height", "fps", "workers", "audio", "timestamps"] as const;

/** Whether a page's host is this machine, where test mode may run. */
export function isLoopback(hostname: string): boolean {
  return hostname === "localhost" || hostname === "127.0.0.1" || hostname === "[::1]" || hostname === "::1";
}

/** Whether a URL fragment asks for test mode, whether or not its parameters are valid. */
export const asksForTest = (hash: string) => hash.replace(/^#/, "").split("&")[0] === "test";

/** Test mode's parameters from a URL fragment, or null if the fragment doesn't ask for it. */
export function parseTestParams(hash: string): TestParams | null {
  if (!asksForTest(hash)) return null;
  const [, ...pairs] = hash.replace(/^#/, "").split("&");
  const params = { ...TEST_DEFAULTS };
  for (const pair of pairs) {
    const [key, value = ""] = pair.split("=", 2);
    if (!KEYS.some((k) => k === key)) {
      throw new Error(`unknown test parameter \`${key}\` (expected frames, width, height, fps, workers, audio or timestamps)`);
    }
    const n = Number(value);
    const ok = key === "fps" ? Number.isFinite(n) && n > 0 : Number.isInteger(n) && n > 0 && /^\d+$/.test(value);
    if (!ok) throw new Error(`test parameter ${key}=${value} must be a positive ${key === "fps" ? "number" : "integer"}`);
    params[key as (typeof KEYS)[number]] = n;
  }
  return params;
}

/** The time of frame `i`, as both hosts compute it: `i / fps` (the WASM call rounds it to f32). */
export const frameTime = (i: number, fps: number) => i / fps;

/** PUTs a file into the page's `results/` directory (see tools/serve.py). */
export async function putResult(base: string, name: string, body: BodyInit): Promise<void> {
  const url = new URL(`results/${name}`, base);
  const response = await fetch(url, { method: "PUT", body });
  if (!response.ok) throw new Error(`PUT ${url} failed: ${response.status} ${response.statusText}`);
}
