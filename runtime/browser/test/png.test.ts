import { expect, test } from "bun:test";
import { inflateSync } from "node:zlib";
import { adler32, crc32, encodePng, zlibStored } from "../src/png.ts";

const text = (s: string) => new TextEncoder().encode(s);

test("CRC-32 and Adler-32 match reference implementations", () => {
  for (const s of ["", "a", "123456789", "The quick brown fox jumps over the lazy dog"]) {
    expect(crc32(text(s))).toBe(Bun.hash.crc32(text(s)));
  }
  expect(crc32(text("1234"), text("56789"))).toBe(0xcbf4_3926);
  expect(adler32(text("Wikipedia"))).toBe(0x11e6_0398);
  expect(adler32(new Uint8Array(0))).toBe(1);
});

test("stored zlib streams inflate back, across several blocks", () => {
  const data = Uint8Array.from({ length: 200_000 }, (_, i) => (i * 31) & 0xff);
  expect(new Uint8Array(inflateSync(zlibStored(data)))).toEqual(data);
  expect(new Uint8Array(inflateSync(zlibStored(new Uint8Array(0))))).toEqual(new Uint8Array(0));
});

/** Reads a PNG's chunks, checking each CRC. */
function chunks(png: Uint8Array): { type: string; data: Uint8Array }[] {
  expect(Array.from(png.subarray(0, 8))).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  const view = new DataView(png.buffer, png.byteOffset);
  const out = [];
  for (let at = 8; at < png.length; ) {
    const len = view.getUint32(at);
    const type = png.subarray(at + 4, at + 8);
    const data = png.subarray(at + 8, at + 8 + len);
    expect(view.getUint32(at + 8 + len)).toBe(crc32(type, data));
    out.push({ type: new TextDecoder().decode(type), data });
    at += 12 + len;
  }
  return out;
}

test("encodes RGBA8 frames that decode to the same pixels", () => {
  const [w, h] = [300, 70]; // over one stored block
  const rgba = Uint8Array.from({ length: w * h * 4 }, (_, i) => (i * 7 + (i >> 10)) & 0xff);
  const parts = chunks(encodePng(w, h, rgba));
  expect(parts.map((c) => c.type)).toEqual(["IHDR", "IDAT", "IEND"]);
  const ihdr = new DataView(parts[0]!.data.buffer, parts[0]!.data.byteOffset);
  expect([ihdr.getUint32(0), ihdr.getUint32(4)]).toEqual([w, h]);
  expect(Array.from(parts[0]!.data.subarray(8))).toEqual([8, 6, 0, 0, 0]);
  const raw = new Uint8Array(inflateSync(parts[1]!.data));
  for (let y = 0; y < h; y++) {
    const row = raw.subarray(y * (w * 4 + 1), (y + 1) * (w * 4 + 1));
    expect(row[0]).toBe(0);
    expect(row.subarray(1)).toEqual(rgba.subarray(y * w * 4, (y + 1) * w * 4));
  }
  expect(() => encodePng(2, 2, new Uint8Array(4))).toThrow("a 2x2 frame has 16 bytes, not 4");
});
