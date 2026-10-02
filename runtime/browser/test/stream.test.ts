import { describe, expect, test } from "bun:test";
import { HEADER_LEN } from "../src/abi.gen.ts";
import { type Command, decode, Sequencer, StreamError } from "../src/stream.ts";
import { Encoder, words } from "./encoder.ts";

const bytes = (...parts: (Uint8Array | number[])[]) => Uint8Array.from(parts.flatMap((p) => Array.from(p)));

/** Expects `f` to throw a StreamError of `kind` whose message contains `text`. */
function expectStreamError(f: () => unknown, kind: StreamError["kind"], text = ""): void {
  try {
    f();
  } catch (e) {
    expect(e).toBeInstanceOf(StreamError);
    expect((e as StreamError).kind).toBe(kind);
    expect((e as StreamError).message).toContain(text);
    return;
  }
  throw new Error(`expected a ${kind} error`);
}

test("golden bytes: the same batch as the Rust crate's golden test", () => {
  const batch = new Encoder()
    .createBuffer(7, 16)
    .writeBuffer(7, 4, Uint8Array.of(1, 2, 3, 4, 5, 6, 7, 8))
    .dispatch(0, [2, 1, 1], [7], Uint8Array.of(0xaa, 0xbb, 0xcc, 0xdd))
    .beginScreenPass([0.0, 0.5, 1.0, 1.0])
    .draw(1, 3, 1, [], Uint8Array.of(1, 0, 0, 0, 2, 0, 0, 0))
    .present()
    .finish();
  const expected = bytes(
    new TextEncoder().encode("WRCS"),
    words([1, 152]),
    words([1, 8, 7, 16]),
    words([2, 20, 7, 4, 8]),
    [1, 2, 3, 4, 5, 6, 7, 8],
    words([3, 32, 0, 2, 1, 1, 1, 7, 4]),
    [0xaa, 0xbb, 0xcc, 0xdd],
    words([4, 16, 0, 0x3f00_0000, 0x3f80_0000, 0x3f80_0000]),
    words([5, 28, 1, 3, 1, 0, 8, 1, 2]),
    words([6, 0]),
  );
  expect(batch).toEqual(expected);
  expect(decode(batch).map((c) => c.op)).toEqual([
    "CreateBuffer",
    "WriteBuffer",
    "Dispatch",
    "BeginScreenPass",
    "Draw",
    "Present",
  ]);
});

test("decodes what it encodes", () => {
  const batch = new Encoder()
    .createBuffer(1, 64)
    .dispatch(2, [4, 2, 1], [1, 3], Uint8Array.of(9, 9, 9, 9))
    .beginScreenPass([0.25, 0.5, 0.75, 1.0])
    .draw(0, 3, 2, [1], new Uint8Array(0))
    .present()
    .finish();
  const plain: unknown[] = decode(batch).map((c) =>
    "uniforms" in c ? { ...c, uniforms: Array.from(c.uniforms) } : c,
  );
  expect(plain).toEqual([
    { op: "CreateBuffer", handle: 1, size: 64 },
    { op: "Dispatch", pipeline: 2, groups: [4, 2, 1], buffers: [1, 3], uniforms: [9, 9, 9, 9] },
    { op: "BeginScreenPass", clear: [0.25, 0.5, 0.75, 1.0] },
    { op: "Draw", pipeline: 0, vertices: 3, instances: 2, buffers: [1], uniforms: [] },
    { op: "Present" },
  ]);
});

test("rejects another version", () => {
  const batch = new Encoder().present().finish();
  batch[4] = 2;
  expectStreamError(() => decode(batch), "WrongVersion", "command stream version 2, but this host reads version 1");
});

describe("rejects malformed batches, with the Rust crate's messages", () => {
  test("too short", () => {
    expectStreamError(() => decode(Uint8Array.from([0x57, 0x52, 0x43])), "TooShort", "needs 12 bytes, has 3");
  });
  test("bad magic", () => {
    const batch = new Encoder().present().finish();
    batch[0] = 0x58;
    expectStreamError(() => decode(batch), "BadMagic", "magic is [88, 82, 67, 83], not `WRCS`");
  });
  test("body length", () => {
    const batch = new Encoder().present().finish();
    batch[8] = 99;
    expectStreamError(() => decode(batch), "BodyLength", "batch declares 99 bytes of commands but has 8");
  });
  test("unknown opcode", () => {
    const batch = new Encoder().present().finish();
    batch[12] = 42;
    expectStreamError(() => decode(batch), "UnknownOpcode", "unknown opcode 42 at byte 12");
  });
  test("a truncated command header", () => {
    const batch = bytes(words([0x5343_5257, 1, 4, 6]));
    expectStreamError(() => decode(batch), "TooShort", "needs 20 bytes, has 16");
  });
  test("a zero-size buffer", () => {
    const batch = new Encoder().createBuffer(1, 4).finish();
    batch[24] = 0;
    expectStreamError(() => decode(batch), "BadPayload", "malformed CreateBuffer at byte 12: size must be a positive multiple of 4");
  });
  test("a uniform length that disagrees with the payload", () => {
    const batch = new Encoder().draw(0, 3, 1, [], Uint8Array.of(1, 2, 3, 4)).finish();
    batch[HEADER_LEN + 8 + 16] = 8;
    expectStreamError(() => decode(batch), "BadPayload", "uniform length doesn't match the payload");
  });
  test("a payload running past the batch", () => {
    const batch = new Encoder().present().finish();
    batch[16] = 4;
    batch[8] = 8; // body length still matches the bytes
    expectStreamError(() => decode(batch), "BadPayload", "payload runs past the end of the batch");
  });
  test("a buffer list running past the payload", () => {
    const batch = new Encoder().dispatch(0, [1, 1, 1], [], new Uint8Array(0)).finish();
    batch[HEADER_LEN + 8 + 16] = 200;
    expectStreamError(() => decode(batch), "BadPayload", "buffer list runs past the payload");
  });
  test("a write whose length disagrees with its data", () => {
    const batch = new Encoder().writeBuffer(1, 0, new Uint8Array(8)).finish();
    batch[HEADER_LEN + 8 + 8] = 4;
    expectStreamError(() => decode(batch), "BadPayload", "data length doesn't match the payload");
  });
});

test("sequencing rules", () => {
  const s = new Sequencer();
  const draw: Command = { op: "Draw", pipeline: 0, vertices: 3, instances: 1, buffers: [], uniforms: new Uint8Array(0) };
  expectStreamError(() => s.step(draw), "Sequence", "Draw out of sequence: a draw must come after BeginScreenPass");
  expectStreamError(() => s.step({ op: "Present" }), "Sequence", "Present must close a screen pass");
  s.step({ op: "BeginScreenPass", clear: [0, 0, 0, 0] });
  expectStreamError(() => s.step({ op: "BeginScreenPass", clear: [0, 0, 0, 0] }), "Sequence", "already open");
  expectStreamError(
    () => s.step({ op: "CreateBuffer", handle: 0, size: 4 }),
    "Sequence",
    "only draws can happen inside a screen pass",
  );
  expectStreamError(() => s.endFrame(), "UnclosedPass", "frame returned with a screen pass still open (no Present)");
  s.step(draw);
  s.step({ op: "Present" });
  s.endFrame();
});
