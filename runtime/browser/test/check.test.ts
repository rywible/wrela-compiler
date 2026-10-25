import { expect, test } from "bun:test";
import { Checker } from "../src/check.ts";
import { NONE, SCREEN } from "../src/abi.gen.ts";
import type { Binding, Command } from "../src/stream.ts";
import { shapes, passOf } from "./fixtures.ts";

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

test("fresh scope rejection restores prior uses and leaves the spare empty", () => {
  const checker = new Checker(shapes());
  for (const handle of [1, 2, 3, 4]) checker.check({ op: "CreateBuffer", handle, size: 64 });
  const binding = (handle: number): Binding => ({ handle, offset: 0, size: 64 });
  const draw = (a: number, b: number): Command => ({
    op: "Draw", pipeline: 0, vertices: 3, instances: 1,
    bindings: [binding(a), binding(b)], uniforms: new Uint8Array(16),
  });
  const dispatch = (a: number, b: number): Command => ({
    op: "Dispatch", pipeline: 1, groups: [1, 1, 1],
    bindings: [binding(a), binding(b)], uniforms: new Uint8Array(16),
  });
  checker.check({ op: "BeginScreenPass", clear: [0, 0, 0, 1] });
  checker.check(draw(1, 2));
  // Test the checker's transaction directly: a late indirect-range failure must restore
  // every old use, even though the new command started a fresh scope.
  expect(() => checker.check({
    op: "DispatchIndirect", pipeline: 1, arguments: 3, offset: 64,
    bindings: [binding(3), binding(4)], uniforms: new Uint8Array(16),
  })).toThrow("reading arguments: 12 bytes at offset 64 overruns buffer 3");
  expect(() => checker.check(draw(2, 1))).toThrow("buffer 2 is used both read-only and read-write in one pass");
  expect(() => checker.check(draw(1, 2))).not.toThrow();
  checker.check({ op: "Present" });
  // Both maps are reused on success and failure; the next scope must start empty each time.
  checker.check(dispatch(3, 4));
  checker.check(dispatch(4, 3));
  expect(() => checker.check({
    op: "DispatchIndirect", pipeline: 1, arguments: 4, offset: 0,
    bindings: [binding(3), binding(4)], uniforms: new Uint8Array(16),
  })).toThrow("buffer 4 is used both read-only and read-write in one dispatch");
  expect(() => checker.check(dispatch(3, 4))).not.toThrow();
});

test("a rejected pass restores its prior attachments and usage scope", () => {
  const m = shapes();
  m.pipelines[0]!.bindings[0] = { binding: 1, kind: "texture", stage: "both", format: "rgba8unorm" };
  m.pipelines.push({
    ...m.pipelines[0]!, name: "read",
    bindings: [m.pipelines[0]!.bindings[0]!, { ...m.pipelines[0]!.bindings[1]!, kind: "read" }],
  });
  const checker = new Checker(m);
  checker.check({ op: "CreateBuffer", handle: 2, size: 64 });
  for (const handle of [5, 6]) {
    checker.check({ op: "CreateTexture", handle, width: 32, height: 32, kind: "2d", layers: 1, levels: 1, format: "rgba8unorm", writable: false });
  }
  checker.check({ op: "CreateTexture", handle: 7, width: 32, height: 31, kind: "2d", layers: 1, levels: 1, format: "depth32float", writable: false });
  const pass = (color: number, depth: number): Command => ({
    op: "BeginPass",
    pass: passOf({ color, keepColor: false, join: false, clear: [0, 0, 0, 1], depth, keepDepth: false, clearDepth: 1 }),
  });
  const draw = (texture: number, pipeline = 0): Command => ({
    op: "Draw", pipeline, vertices: 3, instances: 1,
    bindings: [{ handle: texture, offset: 0, size: 0 }, { handle: 2, offset: 0, size: 64 }], uniforms: new Uint8Array(16),
  });
  checker.check(pass(5, NONE));
  checker.check(draw(6));
  expect(() => checker.check(pass(6, 7))).toThrow("the pass's colour and depth targets differ in size");
  expect(() => checker.check(draw(5))).toThrow("texture 5 is the pass's target");
  expect(() => checker.check(draw(6))).not.toThrow();
  expect(() => checker.check(draw(6, 2))).toThrow("buffer 2 is used both read-only and read-write in one pass");
  checker.check({ op: "EndPass" });
  checker.check(pass(6, NONE));
  expect(() => checker.check(draw(5))).not.toThrow();
});

test("rejected indexed draws restore new binding and index uses", () => {
  const checker = new Checker(shapes());
  for (const handle of [1, 2, 3, 4, 5]) checker.check({ op: "CreateBuffer", handle, size: 64 });
  const binding = (handle: number): Binding => ({ handle, offset: 0, size: 64 });
  const draw = (a: number, b: number): Command => ({
    op: "Draw", pipeline: 0, vertices: 3, instances: 1,
    bindings: [binding(a), binding(b)], uniforms: new Uint8Array(16),
  });
  checker.check({ op: "BeginScreenPass", clear: [0, 0, 0, 1] });
  checker.check(draw(1, 2));
  expect(() => checker.check({
    op: "DrawIndexedIndirect", pipeline: 0, arguments: 4, offset: 0,
    indices: 5, index_offset: 0, index_size: 64,
    bindings: [binding(3), binding(4)], uniforms: new Uint8Array(16),
  })).toThrow("buffer 4 is used both read-only and read-write in one pass");
  // The rejected draw's index buffer may now be written, and its bindings used in reverse.
  expect(() => checker.check(draw(1, 5))).not.toThrow();
  expect(() => checker.check(draw(4, 3))).not.toThrow();
  expect(() => checker.check(draw(2, 1))).toThrow("buffer 2 is used both read-only and read-write in one pass");
});

test("draw rollback scratch preserves uses across successes, failures and passes", () => {
  const checker = new Checker(shapes());
  for (const handle of [1, 2, 3, 4, 5]) checker.check({ op: "CreateBuffer", handle, size: 64 });
  const binding = (handle: number): Binding => ({ handle, offset: 0, size: 64 });
  const draw = (a: number, b: number): Command => ({
    op: "Draw", pipeline: 0, vertices: 3, instances: 1,
    bindings: [binding(a), binding(b)], uniforms: new Uint8Array(16),
  });
  for (const [a, b, c, d] of [[1, 2, 3, 4], [2, 1, 4, 3], [1, 2, 3, 4]] as const) {
    checker.check({ op: "BeginScreenPass", clear: [0, 0, 0, 1] });
    checker.check(draw(a, b));
    expect(() => checker.check({
      op: "DrawIndirect", pipeline: 0, arguments: 5, offset: 64,
      bindings: [binding(c), binding(d)], uniforms: new Uint8Array(16),
    })).toThrow("reading arguments: 16 bytes at offset 64 overruns buffer 5");
    // The failed draw's new uses go; the successful draws' uses stay after later failures.
    checker.check(draw(d, c));
    expect(() => checker.check(draw(b, a))).toThrow(`buffer ${b} is used both read-only and read-write in one pass`);
    expect(() => checker.check(draw(c, d))).toThrow(`buffer ${c} is used both read-only and read-write in one pass`);
    expect(() => checker.check(draw(a, b))).not.toThrow();
    expect(() => checker.check(draw(d, c))).not.toThrow();
    checker.check({ op: "Present" });
  }
});

test("a screen pass requires its depth target to fit the frame", () => {
  const checker = new Checker(shapes());
  checker.setScreenSize(640, 360);
  checker.check({ op: "CreateTexture", handle: 3, width: 640, height: 359, kind: "2d", layers: 1, levels: 1, format: "depth32float", writable: false });
  checker.check({ op: "CreateTexture", handle: 4, width: 640, height: 360, kind: "2d", layers: 1, levels: 1, format: "depth32float", writable: false });
  const pass = (depth: number): Command => ({
    op: "BeginPass",
    pass: passOf({ color: SCREEN, keepColor: false, join: false, clear: [0, 0, 0, 1], depth, keepDepth: false, clearDepth: 1 }),
  });
  expect(() => checker.check(pass(3))).toThrow("the pass's colour and depth targets differ in size");
  expect(() => checker.check(pass(4))).not.toThrow();
  checker.check({ op: "Present" });
});
