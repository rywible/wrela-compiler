import { expect, test } from "bun:test";
import { frameLabels, type GpuTiming } from "../src/gpu.ts";

const t = (frame: number, label: string, nanos: number): GpuTiming =>
  ({ frame, label, nanos, start: 0, end: 0, submission: 0 }) as GpuTiming;

test("each frame's labels are summed and sorted, largest first", () => {
  const labels = frameLabels([t(0, "shadow", 2e6), t(0, "screen", 5e6), t(0, "shadow", 4e6), t(2, "screen", 1e6)], 3);
  expect(labels).toEqual([[["shadow", 6], ["screen", 5]], [], [["screen", 1]]]);
});
