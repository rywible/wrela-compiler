import { expect, test } from "bun:test";
import { Checker } from "../src/check.ts";
import type { Binding, Command } from "../src/stream.ts";
import { shapes } from "./fixtures.ts";

for (const op of ["Dispatch", "Draw"] as const) {
  test(`writable bindings are checked per ${op.toLowerCase()}, including after a rejection`, () => {
    const m = shapes();
    const pipeline = op === "Dispatch" ? 1 : 0;
    for (const b of m.pipelines[pipeline]!.bindings) b.kind = "read_write";
    const checker = new Checker(m);
    for (const handle of [1, 2]) checker.check({ op: "CreateBuffer", handle, size: 64 });
    if (op === "Draw") checker.check({ op: "BeginScreenPass", clear: [0, 0, 0, 1] });
    const command = (bindings: Binding[]): Command => op === "Dispatch"
      ? { op, pipeline, groups: [1, 1, 1], bindings, uniforms: new Uint8Array(16) }
      : { op, pipeline, vertices: 3, instances: 1, bindings, uniforms: new Uint8Array(16) };
    const a = { handle: 1, offset: 0, size: 64 };
    const b = { handle: 2, offset: 0, size: 64 };
    checker.check(command([a, b]));
    checker.check(command([a, b]));
    expect(() => checker.check(command([a, a]))).toThrow(`buffer 1 is bound read-write twice in one ${op.toLowerCase()}`);
    expect(() => checker.check(command([a, b]))).not.toThrow();
  });
}

test("usage conflicts persist within a pass and reset at the next pass", () => {
  const checker = new Checker(shapes());
  for (const handle of [1, 2]) checker.check({ op: "CreateBuffer", handle, size: 64 });
  const a = { handle: 1, offset: 0, size: 64 };
  const b = { handle: 2, offset: 0, size: 64 };
  const draw = (bindings: Binding[]): Command => ({ op: "Draw", pipeline: 0, vertices: 3, instances: 1, bindings, uniforms: new Uint8Array(16) });
  checker.check({ op: "BeginScreenPass", clear: [0, 0, 0, 1] });
  checker.check(draw([a, b]));
  checker.check(draw([a, b]));
  expect(() => checker.check(draw([b, a]))).toThrow("buffer 2 is used both read-only and read-write in one pass");
  // The rejected reversed draw leaves the previous pass scope intact.
  expect(() => checker.check(draw([a, b]))).not.toThrow();
  checker.check({ op: "Present" });
  checker.check({ op: "BeginScreenPass", clear: [0, 0, 0, 1] });
  expect(() => checker.check(draw([b, a]))).not.toThrow();
});
