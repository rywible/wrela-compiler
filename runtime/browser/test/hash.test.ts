import { expect, test } from "bun:test";
import { StateHash } from "../src/hash.ts";

const hex = (s: string) => {
  const h = new StateHash();
  h.update(new TextEncoder().encode(s));
  return h.hex();
};

test("matches the FNV-1a 64 reference values the Rust crate pins", () => {
  expect(hex("")).toBe("cbf29ce484222325");
  expect(hex("a")).toBe("af63dc4c8601ec8c");
  expect(hex("foobar")).toBe("85944171f73967e8");
});

test("incremental updates equal one update", () => {
  const a = new StateHash();
  a.update(new TextEncoder().encode("foo"));
  a.update(new TextEncoder().encode("bar"));
  expect(a.hex()).toBe(hex("foobar"));
});

test("agrees with a BigInt FNV-1a 64 on every byte value", () => {
  const bytes = Uint8Array.from({ length: 4096 }, (_, i) => (i * 167 + (i >> 8)) & 0xff);
  let reference = 0xcbf29ce484222325n;
  for (const b of bytes) reference = ((reference ^ BigInt(b)) * 0x100000001b3n) & 0xffff_ffff_ffff_ffffn;
  const h = new StateHash();
  h.update(bytes);
  expect(h.value()).toBe(reference);
  expect(h.hex()).toBe(reference.toString(16).padStart(16, "0"));
});
