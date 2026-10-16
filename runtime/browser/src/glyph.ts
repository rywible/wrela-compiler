// Glyphs from the system's fonts (`font/glyph`): a character group the program's own fonts
// haven't (an accented letter past Latin-1, a CJK character, an emoji sequence), drawn by the
// browser's text engine, for the program to make its own glyph of. The host only draws: which
// groups to ask for, and what to make of the pixels, are the program's (ui/text.wrela).

import { Reader, Writer } from "./wire.ts";

/** The faces' font stacks, by the face's number: proportional, then monospace. */
const STACKS = [
  'system-ui, -apple-system, "Segoe UI", "Noto Sans", sans-serif',
  'ui-monospace, Menlo, Consolas, "Noto Sans Mono", monospace',
];

/** The most pixels an em is drawn at, and characters a group holds. */
export const MOST_PX = 256;
export const MOST_CHARS = 32;

/** A drawing surface, as the page's OffscreenCanvas gives one (or a test's stand-in). */
export interface Paper {
  getContext(kind: "2d"): Pick<OffscreenCanvasRenderingContext2D, "font" | "fillStyle" | "textBaseline" | "measureText" | "fillText" | "getImageData"> | null;
}

/**
 * Draws a request's group (body: face u8, pixels an em u32, the group's UTF-8) and answers its
 * advance, where its pixels sit from the pen (left, and top above the baseline), their size,
 * whether it has colour of its own (an emoji), then its pixels: RGBA rows from the top, the ink
 * black where it has no colour of its own, its coverage in alpha.
 */
export function glyph(body: Uint8Array, paper: (w: number, h: number) => Paper): Uint8Array<ArrayBuffer> {
  const r = new Reader(body);
  const face = r.u8();
  const px = r.u32();
  const text = r.rest();
  if (face >= STACKS.length) throw new Error(`no font face ${face}`);
  if (px < 4 || px > MOST_PX) throw new Error(`a glyph is drawn at 4 to ${MOST_PX} pixels an em`);
  const chars = [...text].length;
  if (chars === 0 || chars > MOST_CHARS) throw new Error(`a glyph is 1 to ${MOST_CHARS} characters`);
  const font = `${px}px ${STACKS[face]}`;
  const measure = paper(1, 1).getContext("2d");
  if (!measure) throw new Error("this browser can't draw text off screen");
  measure.font = font;
  const m = measure.measureText(text);
  // A texel of room round the ink, so filtering at its edge sees nothing.
  const left = Math.floor(-m.actualBoundingBoxLeft) - 1;
  const top = Math.ceil(m.actualBoundingBoxAscent) + 1;
  const w = Math.max(Math.ceil(m.actualBoundingBoxRight) + 1 - left, 1);
  const h = Math.max(top + Math.ceil(m.actualBoundingBoxDescent) + 1, 1);
  // Its pixels fit one reply (under a request's megabyte).
  if (w * h * 4 > 1_000_000) throw new Error("the glyph is too big");
  const ctx = paper(w, h).getContext("2d");
  if (!ctx) throw new Error("this browser can't draw text off screen");
  ctx.font = font;
  ctx.fillStyle = "#000";
  ctx.textBaseline = "alphabetic";
  ctx.fillText(text, -left, top);
  const pixels = new Uint8Array(ctx.getImageData(0, 0, w, h).data.buffer);
  // Black ink has no colour of its own; an emoji's pixels do.
  let colored = false;
  for (let i = 0; i < pixels.length && !colored; i += 4) {
    if (pixels[i + 3]! > 32 && pixels[i]! + pixels[i + 1]! + pixels[i + 2]! > 48) colored = true;
  }
  return new Writer().f32(m.width).f32(left).f32(top).u32(w).u32(h).u8(colored ? 1 : 0).raw(pixels).bytes();
}
