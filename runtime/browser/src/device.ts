import { FEATURES } from "./abi.gen.ts";
import { LIMIT_SOURCES, type Limits, limitsOf } from "./check.ts";
import { UnsupportedError } from "./errors.ts";

/** The GPU, its adapter or device failed, or reported an error. */
export class GpuError extends Error {
  constructor(why: string) {
    super(`GPU error: ${why}`);
    this.name = "GpuError";
  }
}

/** What a program sees of each device: its optional features (after test mode's mask) and its
 * subgroups' sizes. */
const SEEN = new WeakMap<GPUDevice, { features: string[]; subgroups: [number, number] }>();

/** The limits a program sees of `device` (`wrela.limit`): its own, then its features' bits and
 * its subgroups' sizes. */
export function deviceLimits(device: GPUDevice): Limits {
  const seen = SEEN.get(device) ?? { features: ["indirect-first-instance"], subgroups: [0, 0] as [number, number] };
  return limitsOf(device.limits, seen.features, seen.subgroups);
}

/** Opens a hardware adapter with its own limits and every optional feature it has, as the native
 * host does. `keep` (test mode's `features=`) is what the program may see of them: null all,
 * else `none` or a comma list. */
export async function openDevice(fatal: (error: Error) => void, keep: string | null = null): Promise<GPUDevice> {
  const adapter = await navigator.gpu?.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) throw new UnsupportedError("WebGPU isn't available here (no adapter)");
  if (adapter.info.isFallbackAdapter !== false) throw new UnsupportedError("hardware GPU required; WebGPU selected a fallback adapter");
  if (!adapter.features.has("indirect-first-instance")) throw new UnsupportedError("the GPU doesn't support indirect-first-instance");
  const requiredLimits: Record<string, number> = {};
  for (const name of LIMIT_SOURCES.flat()) requiredLimits[name] = adapter.limits[name];
  // Every optional feature wherever the adapter has it (timestamps too: a program may ask for its
  // work's time, `std::gpu::timed`, and a timed run times every pass). None is required: a
  // program checks `std::gpu::features()` before it records what needs one.
  // A mask (test mode's `features=`) holds for the device itself, so pipeline creation and the
  // executor see what the program sees; timestamps stay, which a profile's timing needs.
  const kept = keep === null ? null : keep === "none" ? [] : keep.split(",");
  const wanted = (f: string) => f === "indirect-first-instance" || kept === null || kept.includes(f);
  const requiredFeatures = FEATURES.filter((f) => adapter.features.has(f) && (wanted(f) || f === "timestamp-query")) as GPUFeatureName[];
  const device = await adapter.requestDevice({ requiredLimits, requiredFeatures });
  const features = requiredFeatures.filter(wanted);
  const info = adapter.info as GPUAdapterInfo & { subgroupMinSize?: number; subgroupMaxSize?: number };
  const subgroups: [number, number] = features.includes("subgroups") ? [info.subgroupMinSize ?? 0, info.subgroupMaxSize ?? 0] : [0, 0];
  SEEN.set(device, { features, subgroups });
  device.addEventListener("uncapturederror", (e) => fatal(new GpuError(e.error.message)));
  device.lost.then((info) => {
    if (info.reason !== "destroyed") fatal(new GpuError(`device lost: ${info.message}`));
  });
  return device;
}
