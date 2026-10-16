// Glyphs from the system's fonts (src/glyph.ts): a character group drawn, measured and answered.

import { expect, test } from "bun:test";
import { glyph, type Paper } from "../src/glyph.ts";
import { Reader, Writer } from "../src/wire.ts";

/** A surface whose text measures as given and whose pixels are `fill`'s. */
function paper(fill: [number, number, number, number], log: string[]) {
  return (w: number, h: number): Paper => ({
    getContext: () => ({
      font: "",
      fillStyle: "",
      textBaseline: "alphabetic",
      measureText: (t: string) => ({ width: 20, actualBoundingBoxLeft: -1.5, actualBoundingBoxRight: 18.2, actualBoundingBoxAscent: 22.5, actualBoundingBoxDescent: 4 }) as TextMetrics,
      fillText(t: string, x: number, y: number) {
        log.push(`${t} at ${x},${y} on ${w}x${h}`);
      },
      getImageData: (_x: number, _y: number, gw: number, gh: number) => ({ data: new Uint8ClampedArray(gw * gh * 4).map((_, i) => fill[i % 4]!) }) as ImageData,
    }) as unknown as ReturnType<Paper["getContext"]>,
  });
}

const request = (face: number, px: number, text: string) => {
  const w = new Writer().u8(face).u32(px);
  return new Uint8Array([...w.bytes(), ...new TextEncoder().encode(text)]);
};

test("a group is drawn on its baseline, with a texel round its ink, and answered with its metrics", () => {
  const log: string[] = [];
  const r = new Reader(glyph(request(0, 128, "👍🏽"), paper([0, 0, 0, 200], log)));
  const [advance, left, top, w, h, colored] = [r.f32(), r.f32(), r.f32(), r.u32(), r.u32(), r.u8()];
  expect([advance, left, top]).toEqual([20, 0, 24]);
  // From one texel left of the ink (1.5 right of the pen, floored) to one past its right.
  expect([w, h]).toEqual([20, 29]);
  expect(colored).toBe(0);
  expect(log).toEqual([`👍🏽 at 0,24 on 20x29`]);
  expect(r.bytes.length).toBe(4 * 5 + 1 + w * h * 4);
});

test("ink with colour of its own is an emoji's", () => {
  const r = new Reader(glyph(request(1, 64, "😀"), paper([250, 200, 40, 255], [])));
  r.f32(); r.f32(); r.f32(); r.u32(); r.u32();
  expect(r.u8()).toBe(1);
});

test("a request out of bounds says why", () => {
  const p = paper([0, 0, 0, 255], []);
  expect(() => glyph(request(2, 64, "a"), p)).toThrow("no font face 2");
  expect(() => glyph(request(0, 1000, "a"), p)).toThrow("4 to 256 pixels");
  expect(() => glyph(request(0, 64, ""), p)).toThrow("1 to 32 characters");
});
