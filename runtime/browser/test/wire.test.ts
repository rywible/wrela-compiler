import { expect, test } from "bun:test";
import { Reader, Writer } from "../src/wire.ts";

test("serialized strings keep an initial byte-order mark as text", () => {
  const text = "\uFEFFtext";
  expect(new Reader(new Writer().str(text).bytes()).str()).toBe(text);
  expect(new Reader(new TextEncoder().encode(text)).rest()).toBe(text);
});
