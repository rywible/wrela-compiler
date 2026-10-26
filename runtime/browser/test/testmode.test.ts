import { expect, test } from "bun:test";
import { asksForTest, frameTime, inputUrl, isLoopback, mayTest, parseTestParams, putResult, summaryLines, TEST_DEFAULTS } from "../src/testmode.ts";

test("only #test fragments ask for test mode", () => {
  expect(parseTestParams("")).toBeNull();
  expect(parseTestParams("#run")).toBeNull();
  expect(parseTestParams("#testing")).toBeNull();
  expect(parseTestParams("#test")).toEqual(TEST_DEFAULTS);
  // A bad parameter still asks for test mode, so the failure ends the test.
  expect(asksForTest("#test&frame=3")).toBe(true);
  expect(asksForTest("#testing")).toBe(false);
});

test("reads every parameter", () => {
  expect(parseTestParams("#test&frames=3&width=640&height=360&fps=29.97&workers=4&audio=750&live=9")).toEqual({
    frames: 3,
    width: 640,
    height: 360,
    fps: 29.97,
    workers: 4,
    audio: 750,
    live: 9,
    timestamps: 0,
    input: "",
    features: null,
    latency: 0,
    keylatency: 0,
    nohash: 0,
    ticklog: 0,
    inflight: 0,
    snap: 0,
    snapfrom: 0,
    paced: 0,
    tickdelay: 0,
    framedelay: 0,
    salt: 0,
    saturate: 0,
    clip: 0,
    clipfrom: 0,
    checks: 1,
    show: 0,
  });
  expect(parseTestParams("#test&clip=600&clipfrom=300")).toEqual({ ...TEST_DEFAULTS, clip: 600, clipfrom: 300 });
  expect(parseTestParams("#test&input=scripts/a.json&latency=40")).toEqual({ ...TEST_DEFAULTS, input: "scripts/a.json", latency: 40 });
  expect(parseTestParams("#test&paced=1&tickdelay=30&framedelay=5&keylatency=20&salt=7&saturate=1")).toEqual({
    ...TEST_DEFAULTS,
    salt: 7,
    saturate: 1,
    paced: 1,
    tickdelay: 30,
    framedelay: 5,
    keylatency: 20,
  });
});

test("rejects unknown or invalid parameters", () => {
  expect(() => parseTestParams("#test&frame=3")).toThrow("unknown test parameter `frame`");
  expect(() => parseTestParams("#test&width=0")).toThrow("width=0 must be a positive integer");
  expect(() => parseTestParams("#test&width=1.5")).toThrow("must be a positive integer");
  expect(() => parseTestParams("#test&height=")).toThrow("must be a positive integer");
  expect(() => parseTestParams("#test&fps=-1")).toThrow("fps=-1 must be a positive number");
  expect(() => parseTestParams("#test&input=../x.json")).toThrow("must be a path relative to the page");
  expect(() => parseTestParams("#test&input=http://x/y")).toThrow("must be a path relative to the page");
  expect(() => parseTestParams("#test&input=..\\x.json")).toThrow("must be a path relative to the page");
  expect(() => parseTestParams("#test&input=x.json?other=1")).toThrow("must be a path relative to the page");
});

test("an input script's percent escapes stay literal and under the page", () => {
  const path = parseTestParams("#test&input=%2e%2e/keys.json")!.input;
  expect(inputUrl("http://localhost/game/index.html", path).href).toBe("http://localhost/game/%252e%252e/keys.json");
});

test("an input script's whitespace stays literal and under the page", () => {
  for (const [path, escaped] of [[" /keys.json", "%20/keys.json"], ["\t/keys.json", "%09/keys.json"]]) {
    const parsed = parseTestParams(`#test&input=${path}`)!.input;
    expect(inputUrl("http://localhost/game/index.html", parsed).href).toBe(`http://localhost/game/${escaped}`);
  }
});

test("parameter values retain equals signs instead of silently truncating them", () => {
  const input = parseTestParams("#test&input=keys=demo.json")!.input;
  expect(input).toBe("keys=demo.json");
  expect(inputUrl("http://localhost/game/index.html", input).href).toBe("http://localhost/game/keys%3Ddemo.json");
  expect(() => parseTestParams("#test&fps=60=extra")).toThrow("must be a positive number");
});

test("frame times are i / fps, rounded to f32 like the native host's", () => {
  expect(frameTime(30, 60)).toBe(0.5);
  // f64 division then f32 rounding: what the WASM call does to the argument.
  expect(Math.fround(frameTime(1, 60))).toBe(Math.fround(1 / 60));
});

test("only a page served from this machine may enter test mode", () => {
  for (const host of ["localhost", "127.0.0.1", "[::1]"]) expect(isLoopback(host)).toBe(true);
  for (const host of ["example.com", "192.168.1.5", "localhost.example.com", ""]) expect(isLoopback(host)).toBe(false);
});

test("test results stay at the page's result endpoint", async () => {
  const saved = globalThis.fetch;
  let request: { url: string; init: RequestInit | undefined } | undefined;
  globalThis.fetch = (async (url: URL, init?: RequestInit) => {
    request = { url: url.href, init };
    return new Response(null, { status: 204 });
  }) as typeof fetch;
  try {
    await putResult("http://127.0.0.1:8000/game/index.html#test", "log.txt", "private result");
    expect(request).toEqual({
      url: "http://127.0.0.1:8000/game/results/log.txt",
      init: { method: "PUT", body: "private result", redirect: "error" },
    });
  } finally { globalThis.fetch = saved; }
});

test("checks=0 turns the checker off, as a release build plays; anything but 0 or 1 is refused", () => {
  expect(parseTestParams("#test&checks=0")!.checks).toBe(0);
  expect(parseTestParams("#test")!.checks).toBe(1);
  expect(() => parseTestParams("#test&checks=2")).toThrow("checks=2 must be 0 or 1");
});

test("test mode runs on this machine and on a private network's addresses, never a name", () => {
  for (const h of ["localhost", "127.0.0.1", "10.0.0.7", "172.16.4.2", "172.31.255.1", "192.168.1.20"]) expect(mayTest(h)).toBe(true);
  for (const h of ["example.com", "8.8.8.8", "172.32.0.1", "192.169.0.1", "10.0.0.256", "192.168.1.20.example.com"]) expect(mayTest(h)).toBe(false);
});

test("show=1's summary names the intervals, when it was playable, and the adapter", () => {
  const lines = summaryLines(
    { began_ms: [1000, 1016, 1050, 1066], printed: ["hi", "playable"], printed_in: [0, 2], opened_ms: 900, pipelines_ms: 12.5 },
    "apple metal-3",
  );
  expect(lines).toEqual([
    "4 frames; intervals p50 16.0 ms, p95 34.0 ms, p99 34.0 ms; over 33 ms: 1",
    "first frame 100.0 ms after the page opened",
    "playable 150.0 ms",
    "pipelines 12.5 ms",
    "apple metal-3",
  ]);
});
