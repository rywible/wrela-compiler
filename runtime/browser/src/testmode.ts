// Test mode: `index.html#test&frames=60&width=1920&height=1080&fps=60` runs a fixed number of
// frames at fixed times and canvas size, then saves the last frame and the state hash to
// `results/` for tools/headless.py. It's part of the shipped bundle, so the agreement test runs
// the exact bytes a game ships, but only a page served from this machine (tools/serve.py and
// tools/headless.py bind 127.0.0.1) enters it: a game's public URL ignores `#test`.

export interface TestParams {
  frames: number;
  width: number;
  height: number;
  fps: number;
}

export const TEST_DEFAULTS: TestParams = { frames: 60, width: 1920, height: 1080, fps: 60 };

/** Whether a page's host is this machine, where test mode may run. */
export function isLoopback(hostname: string): boolean {
  return hostname === "localhost" || hostname === "127.0.0.1" || hostname === "[::1]" || hostname === "::1";
}

/** Test mode's parameters from a URL fragment, or null if the fragment doesn't ask for it. */
export function parseTestParams(hash: string): TestParams | null {
  const [mode, ...pairs] = hash.replace(/^#/, "").split("&");
  if (mode !== "test") return null;
  const params = { ...TEST_DEFAULTS };
  for (const pair of pairs) {
    const [key, value = ""] = pair.split("=", 2);
    if (key !== "frames" && key !== "width" && key !== "height" && key !== "fps") {
      throw new Error(`unknown test parameter \`${key}\` (expected frames, width, height or fps)`);
    }
    const n = Number(value);
    const ok = key === "fps" ? Number.isFinite(n) && n > 0 : Number.isInteger(n) && n > 0 && /^\d+$/.test(value);
    if (!ok) throw new Error(`test parameter ${key}=${value} must be a positive ${key === "fps" ? "number" : "integer"}`);
    params[key] = n;
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
