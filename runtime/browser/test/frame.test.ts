import { expect, test } from "bun:test";
import { fitSize, GameClock } from "../src/frame.ts";

test("game time counts only visible running", () => {
  const clock = new GameClock();
  expect([clock.tick(1000), clock.tick(1016), clock.tick(1032)]).toEqual([0, 0.016, 0.032]);
  // Hidden for 60 s, with no frames (a browser may give a hidden page none).
  clock.setVisible(false);
  clock.setVisible(true);
  expect([clock.tick(61_032), clock.tick(61_048)]).toEqual([0.032, 0.048]);
  // Hidden with frames still running: they run nothing.
  clock.setVisible(false);
  expect([clock.tick(61_064), clock.tick(70_000)]).toEqual([null, null]);
  clock.setVisible(true);
  expect([clock.tick(70_016), clock.tick(70_032)]).toEqual([0.048, 0.064]);
});

test("a hidden page's first frames run nothing", () => {
  const clock = new GameClock();
  clock.setVisible(false);
  expect(clock.tick(5)).toBeNull();
  clock.setVisible(true);
  expect([clock.tick(9000), clock.tick(9016)]).toEqual([0, 0.016]);
});

test("a canvas too large for the device is scaled down as a whole", () => {
  expect(fitSize(1920, 1080, 8192)).toEqual({ width: 1920, height: 1080 });
  expect(fitSize(0, -3, 8192)).toEqual({ width: 1, height: 1 });
  // Two 5K displays side by side: the shape is kept.
  expect(fitSize(10240, 2880, 8192)).toEqual({ width: 8192, height: 2304 });
  expect(fitSize(2880, 10240, 8192)).toEqual({ width: 2304, height: 8192 });
  expect(fitSize(100_000, 1, 8192)).toEqual({ width: 8192, height: 1 });
});
