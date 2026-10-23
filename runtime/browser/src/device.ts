import { LIMIT_SOURCES } from "./check.ts";
import { UnsupportedError } from "./errors.ts";

/** The GPU, its adapter or device failed, or reported an error. */
export class GpuError extends Error {
  constructor(why: string) {
    super(`GPU error: ${why}`);
    this.name = "GpuError";
  }
}

/** Opens a hardware adapter with its own limits, as the native host does. */
export async function openDevice(fatal: (error: Error) => void): Promise<GPUDevice> {
  const adapter = await navigator.gpu?.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) throw new UnsupportedError("WebGPU isn't available here (no adapter)");
  if (adapter.info.isFallbackAdapter !== false) throw new UnsupportedError("hardware GPU required; WebGPU selected a fallback adapter");
  if (!adapter.features.has("indirect-first-instance")) throw new UnsupportedError("the GPU doesn't support indirect-first-instance");
  const requiredLimits: Record<string, number> = {};
  for (const name of LIMIT_SOURCES.flat()) requiredLimits[name] = adapter.limits[name];
  const requiredFeatures: GPUFeatureName[] = ["indirect-first-instance"];
  // Timestamps wherever the adapter has them: a program may ask for its work's time
  // (`std::gpu::timed`), and a timed run times every pass.
  if (adapter.features.has("timestamp-query")) requiredFeatures.push("timestamp-query");
  const device = await adapter.requestDevice({ requiredLimits, requiredFeatures });
  device.addEventListener("uncapturederror", (e) => fatal(new GpuError(e.error.message)));
  device.lost.then((info) => {
    if (info.reason !== "destroyed") fatal(new GpuError(`device lost: ${info.message}`));
  });
  return device;
}
