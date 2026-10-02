import { expect, test } from "bun:test";
import { frameTime, parseTestParams, TEST_DEFAULTS } from "../src/testmode.ts";

test("only #test fragments ask for test mode", () => {
  expect(parseTestParams("")).toBeNull();
  expect(parseTestParams("#run")).toBeNull();
  expect(parseTestParams("#testing")).toBeNull();
  expect(parseTestParams("#test")).toEqual(TEST_DEFAULTS);
});

test("reads every parameter", () => {
  expect(parseTestParams("#test&frames=3&width=640&height=360&fps=29.97")).toEqual({
    frames: 3,
    width: 640,
    height: 360,
    fps: 29.97,
  });
});

test("rejects unknown or invalid parameters", () => {
  expect(() => parseTestParams("#test&frame=3")).toThrow("unknown test parameter `frame`");
  expect(() => parseTestParams("#test&width=0")).toThrow("width=0 must be a positive integer");
  expect(() => parseTestParams("#test&width=1.5")).toThrow("must be a positive integer");
  expect(() => parseTestParams("#test&height=")).toThrow("must be a positive integer");
  expect(() => parseTestParams("#test&fps=-1")).toThrow("fps=-1 must be a positive number");
});

test("frame times are i / fps, rounded to f32 like the native host's", () => {
  expect(frameTime(30, 60)).toBe(0.5);
  // f64 division then f32 rounding: what the WASM call does to the argument.
  expect(Math.fround(frameTime(1, 60))).toBe(Math.fround(1 / 60));
});
