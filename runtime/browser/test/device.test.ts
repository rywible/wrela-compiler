import { afterEach, expect, mock, test } from "bun:test";
import { featureNames } from "../src/check.ts";
import { deviceLimits, openDevice } from "../src/device.ts";
import { UnsupportedError } from "../src/errors.ts";

const original = Object.getOwnPropertyDescriptor(navigator, "gpu");
afterEach(() => {
  if (original) Object.defineProperty(navigator, "gpu", original);
  else delete (navigator as unknown as Record<string, unknown>).gpu;
});

test("software and unclassified adapters fail before a device is requested", async () => {
  const requestDevice = mock(() => Promise.resolve({}));
  for (const isFallbackAdapter of [true, undefined]) {
    Object.defineProperty(navigator, "gpu", { configurable: true, value: {
      requestAdapter: async () => ({ info: { isFallbackAdapter }, requestDevice }),
    } });
    await expect(openDevice(() => {})).rejects.toThrow("hardware GPU required");
    await expect(openDevice(() => {})).rejects.toBeInstanceOf(UnsupportedError);
  }
  expect(requestDevice).not.toHaveBeenCalled();
});

test("a missing adapter reports its cause, as a machine that can't run a program", async () => {
  Object.defineProperty(navigator, "gpu", { configurable: true, value: { requestAdapter: async () => null } });
  await expect(openDevice(() => {})).rejects.toThrow("no adapter");
  await expect(openDevice(() => {})).rejects.toBeInstanceOf(UnsupportedError);
});

test("hardware adapters retain limits and enable indirect first instances and timestamps", async () => {
  const device = { addEventListener: mock(() => {}), lost: new Promise(() => {}) };
  const requestDevice = mock(async (_descriptor: GPUDeviceDescriptor) => device);
  const requestAdapter = mock(async (_options: GPURequestAdapterOptions) => ({ info: { isFallbackAdapter: false }, limits: {},
    features: new Set(["timestamp-query", "indirect-first-instance"]), requestDevice }));
  Object.defineProperty(navigator, "gpu", { configurable: true, value: { requestAdapter } });
  expect(await openDevice(() => {})).toBe(device as unknown as GPUDevice);
  expect(requestAdapter).toHaveBeenCalledWith({ powerPreference: "high-performance" });
  expect(requestDevice.mock.calls[0]).toEqual([{ requiredLimits: expect.any(Object), requiredFeatures: ["timestamp-query", "indirect-first-instance"] }]);
});

test("an adapter without indirect first instances fails before requesting a device", async () => {
  const requestDevice = mock(() => Promise.resolve({}));
  Object.defineProperty(navigator, "gpu", { configurable: true, value: {
    requestAdapter: async () => ({ info: { isFallbackAdapter: false }, features: new Set(), requestDevice }),
  } });
  await expect(openDevice(() => {})).rejects.toThrow("indirect-first-instance");
  await expect(openDevice(() => {})).rejects.toBeInstanceOf(UnsupportedError);
  expect(requestDevice).not.toHaveBeenCalled();
});

test("features=none hides every optional feature from the program, as the native host's mask does", async () => {
  const device = { addEventListener: mock(() => {}), lost: new Promise(() => {}), limits: {} };
  const requestDevice = mock(async (_descriptor: GPUDeviceDescriptor) => device);
  const has = ["shader-f16", "subgroups", "texture-compression-bc", "timestamp-query", "indirect-first-instance"];
  const requestAdapter = mock(async () => ({ info: { isFallbackAdapter: false, subgroupMinSize: 32, subgroupMaxSize: 32 }, limits: {},
    features: new Set(has), requestDevice }));
  Object.defineProperty(navigator, "gpu", { configurable: true, value: { requestAdapter } });
  const all = await openDevice(() => {});
  expect(featureNames(deviceLimits(all)[10]!)).toEqual(["shader-f16", "subgroups", "texture-compression-bc", "timestamp-query", "indirect-first-instance"]);
  expect(deviceLimits(all)[11]).toBe(32);
  const none = await openDevice(() => {}, "none");
  expect(featureNames(deviceLimits(none)[10]!)).toEqual(["indirect-first-instance"]);
  expect(deviceLimits(none)[12]).toBe(0);
  const some = await openDevice(() => {}, "subgroups");
  expect(featureNames(deviceLimits(some)[10]!)).toEqual(["subgroups", "indirect-first-instance"]);
});
