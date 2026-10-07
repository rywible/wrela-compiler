import { expect, test } from "bun:test";
import { fitSize, frameSeconds, VisibleClock } from "../src/frame.ts";

test("visible time stands still while the page is hidden", () => {
  const clock = VisibleClock.create();
  clock.startAt(1000);
  expect([1000, 1016, 1032].map((now) => frameSeconds(clock, now))).toEqual([0, 0.016, 0.032]);
  // Hidden for 60 s, with no frames (a browser may give a hidden page none).
  clock.setVisible(false, 1040);
  clock.setVisible(true, 61_040);
  expect([61_048, 61_064].map((now) => frameSeconds(clock, now))).toEqual([0.048, 0.064]);
  // Hidden with frames still running: they run nothing, and the time holds.
  clock.setVisible(false, 61_070);
  expect([61_080, 70_000].map((now) => frameSeconds(clock, now))).toEqual([null, null]);
  expect(clock.seconds(70_000)).toBe(0.07);
  clock.setVisible(true, 70_000);
  expect(frameSeconds(clock, 70_016)).toBe(0.086);
});

test("a second thread reads the same visible time", () => {
  const clock = VisibleClock.create();
  clock.setVisible(false, 500);
  clock.setVisible(true, 2500);
  clock.startAt(2600);
  const other = new VisibleClock(clock.buffer);
  expect(other.visible).toBe(true);
  expect(other.at(3000)).toBe(clock.at(3000));
  expect(other.at(3000)).toBe(1000);
  expect(other.seconds(3000)).toBe(0.4);
  // Showing an already visible page changes nothing.
  clock.setVisible(true, 9000);
  expect(other.at(9000)).toBe(7000);
});

test("a canvas too large for the device is scaled down as a whole", () => {
  expect(fitSize(1920, 1080, 8192)).toEqual({ width: 1920, height: 1080 });
  expect(fitSize(0, -3, 8192)).toEqual({ width: 1, height: 1 });
  // Two 5K displays side by side: the shape is kept.
  expect(fitSize(10240, 2880, 8192)).toEqual({ width: 8192, height: 2304 });
  expect(fitSize(2880, 10240, 8192)).toEqual({ width: 2304, height: 8192 });
  expect(fitSize(100_000, 1, 8192)).toEqual({ width: 8192, height: 1 });
});
