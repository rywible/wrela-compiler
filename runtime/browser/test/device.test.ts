import { afterEach, expect, mock, test } from "bun:test";
import { openDevice } from "../src/device.ts";

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
    await expect(openDevice(false, () => {})).rejects.toThrow("hardware GPU required");
  }
  expect(requestDevice).not.toHaveBeenCalled();
});

test("a missing adapter reports its cause", async () => {
  Object.defineProperty(navigator, "gpu", { configurable: true, value: { requestAdapter: async () => null } });
  await expect(openDevice(false, () => {})).rejects.toThrow("no adapter");
});

test("hardware adapters retain limits and timestamp features", async () => {
  const device = { addEventListener: mock(() => {}), lost: new Promise(() => {}) };
  const requestDevice = mock(async (_descriptor: GPUDeviceDescriptor) => device);
  const requestAdapter = mock(async (_options: GPURequestAdapterOptions) => ({ info: { isFallbackAdapter: false }, limits: {},
    features: new Set(["timestamp-query"]), requestDevice }));
  Object.defineProperty(navigator, "gpu", { configurable: true, value: { requestAdapter } });
  expect(await openDevice(true, () => {})).toBe(device as unknown as GPUDevice);
  expect(requestAdapter).toHaveBeenCalledWith({ powerPreference: "high-performance" });
  expect(requestDevice.mock.calls[0]).toEqual([{ requiredLimits: expect.any(Object), requiredFeatures: ["timestamp-query"] }]);
});
