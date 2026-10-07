import { expect, test } from "bun:test";
import { asksForTest, frameTime, isLoopback, parseTestParams, TEST_DEFAULTS } from "../src/testmode.ts";

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
  expect(parseTestParams("#test&frames=3&width=640&height=360&fps=29.97&workers=4&audio=750")).toEqual({
    frames: 3,
    width: 640,
    height: 360,
    fps: 29.97,
    workers: 4,
    audio: 750,
    timestamps: 0,
    input: "",
    latency: 0,
    keylatency: 0,
    nohash: 0,
    paced: 0,
    tickdelay: 0,
    framedelay: 0,
    salt: 0,
    saturate: 0,
  });
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
