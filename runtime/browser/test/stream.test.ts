// The command stream's decoding, sequencing and messages are checked against the ABI's
// vectors (vectors.test.ts). Here: the test encoder the other tests build batches with.

import { expect, test } from "bun:test";
import { Encoder } from "./encoder.ts";
import { vectors } from "./fixtures.ts";

test("the test encoder writes the vectors' golden batch", () => {
  const golden = vectors.batches.find((b) => b.name === "golden")!.bytes;
  const batch = new Encoder()
    .createBuffer(7, 16)
    .writeBuffer(7, 4, Uint8Array.of(1, 2, 3, 4, 5, 6, 7, 8))
    .dispatch(0, [2, 1, 1], [7], Uint8Array.of(0xaa, 0xbb, 0xcc, 0xdd))
    .beginScreenPass([0.0, 0.5, 1.0, 1.0])
    .draw(1, 3, 1, [], Uint8Array.of(1, 0, 0, 0, 2, 0, 0, 0))
    .present()
    .destroyBuffer(7)
    .finish();
  expect(Buffer.from(batch).toString("hex")).toBe(golden);
});
