// The command stream's decoding, sequencing and messages are checked against the ABI's
// vectors (vectors.test.ts). Here: the test encoder the other tests build batches with.

import { expect, test } from "bun:test";
import { NONE } from "../src/abi.gen.ts";
import { Encoder } from "./encoder.ts";
import { vectors } from "./fixtures.ts";
import { Opcode } from "../src/abi.gen.ts";
import { f32bits } from "./encoder.ts";
import { onePass } from "../src/stream.ts";

test("the test encoder writes the vectors' golden batch", () => {
  const golden = vectors.batches.find((b) => b.name === "golden")!.bytes;
  const offscreen = { ...onePass(8, false, [0, 0, 0, 1]), depth: 9 };
  const batch = new Encoder()
    .createBuffer(7, 16)
    .writeBuffer(7, 4, Uint8Array.of(1, 2, 3, 4, 5, 6, 7, 8))
    .dispatch(0, [2, 1, 1], [[7, 0, 16]], Uint8Array.of(0xaa, 0xbb, 0xcc, 0xdd))
    .beginScreenPass([0.0, 0.5, 1.0, 1.0])
    .draw(1, 3, 1, [], Uint8Array.of(1, 0, 0, 0, 2, 0, 0, 0))
    .present()
    .destroyBuffer(7)
    .copyBuffer(1, 4, 2, 8, 12)
    .createTexture(8, 2, 1, "rgba8unorm")
    .writeTexture(8, [0, 0], [2, 1], Uint8Array.of(1, 2, 3, 4, 5, 6, 7, 8))
    .destroyTexture(8)
    .createSampler(10, true, false, "less")
    .destroySampler(10)
    .beginPass(offscreen)
    .endPass()
    .dispatchIndirect(0, 3, 4, [[5, 0, 0]], new Uint8Array(0))
    .drawIndirect(1, 3, 16, [], new Uint8Array(0))
    .readBuffer(1, 3, 0, 8)
    .storageRead(2, "saves/slot1")
    .storageWrite(3, "saves/a", Uint8Array.of(1, 2, 3, 4, 5))
    .fetch(4, "data/level.bin")
    .post(5, "notes/write", Uint8Array.of(123, 125))
    .drawIndexedIndirect(1, [3, 0, 12], 3, 16, [], new Uint8Array(0))
    .label("terrain")
    .prepare(6, "../floor")
    .unprepare("../floor")
    .handOver(7, "next")
    .hostRequest(8, "test/echo", Uint8Array.of(3, 2, 1))
    .timed(9, "shadows")
    .words(Opcode.CopyTexture, [8, 1, 0, 0, 0, 12, 0, 3, 4, 4, 2, 2])
    .words(Opcode.CopyTextureToBuffer, [8, 0, 0, 1, 1, 2, 2, 3, 256, 256])
    .words(Opcode.CopyBufferToTexture, [3, 0, 256, 8, 2, 0, 0, 0, 4, 4])
    .words(Opcode.SetScissor, [1, 2, 3, 4])
    .words(Opcode.SetViewport, [0, 0, 64, 32, 0, 1].map(f32bits))
    .words(Opcode.DrawIndexed, [1, 3, 0, 12, 6, 2, 1, 0xffff_ffff, 4, 0, 0])
    .words(Opcode.MultiDrawIndirect, [1, 3, 16, 4, NONE, 0, 0, 0])
    .words(Opcode.MultiDrawIndexedIndirect, [1, 3, 0, 12, 5, 0, 2, 6, 8, 0, 0])
    .words(Opcode.ConfigureScreen, [1, 1])
    .finish();
  expect(Buffer.from(batch).toString("hex")).toBe(golden);
  expect(NONE).toBe(0xffffffff);
});
