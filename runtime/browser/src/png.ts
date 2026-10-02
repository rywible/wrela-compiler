// A minimal PNG encoder for RGBA8 frames: no filtering, and deflate's stored (uncompressed)
// blocks, so it needs no compressor. Files are about as big as the raw pixels, which is fine
// for test output.

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb8_8320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

/** CRC-32 (ISO 3309, as PNG uses), over `parts` in order. */
export function crc32(...parts: Uint8Array[]): number {
  let c = 0xffff_ffff;
  for (const bytes of parts) {
    for (let i = 0; i < bytes.length; i++) c = CRC_TABLE[(c ^ bytes[i]!) & 0xff]! ^ (c >>> 8);
  }
  return (c ^ 0xffff_ffff) >>> 0;
}

/** Adler-32 (RFC 1950), zlib's checksum. */
export function adler32(bytes: Uint8Array): number {
  let a = 1;
  let b = 0;
  // 5552 is zlib's NMAX: the most bytes whose running sums stay below 2^32, so the modulo can
  // stay out of the inner loop.
  for (let i = 0; i < bytes.length; ) {
    const end = Math.min(i + 5552, bytes.length);
    for (; i < end; i++) {
      a += bytes[i]!;
      b += a;
    }
    a %= 65521;
    b %= 65521;
  }
  return ((b << 16) | a) >>> 0;
}

const MAX_STORED = 65535;

/** A zlib stream holding `data` in stored deflate blocks. */
export function zlibStored(data: Uint8Array): Uint8Array<ArrayBuffer> {
  const blocks = Math.max(1, Math.ceil(data.length / MAX_STORED));
  const out = new Uint8Array(2 + blocks * 5 + data.length + 4);
  const view = new DataView(out.buffer);
  out[0] = 0x78; // deflate, 32 KiB window
  out[1] = 0x01; // no preset dictionary, fastest; (0x78 << 8 | 0x01) % 31 === 0
  let at = 2;
  for (let b = 0; b < blocks; b++) {
    const chunk = data.subarray(b * MAX_STORED, Math.min((b + 1) * MAX_STORED, data.length));
    out[at] = b === blocks - 1 ? 1 : 0; // BFINAL, BTYPE = 00 (stored)
    view.setUint16(at + 1, chunk.length, true);
    view.setUint16(at + 3, ~chunk.length & 0xffff, true);
    out.set(chunk, at + 5);
    at += 5 + chunk.length;
  }
  view.setUint32(at, adler32(data));
  return out;
}

function chunk(type: string, data: Uint8Array): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(12 + data.length);
  const view = new DataView(out.buffer);
  const tag = new TextEncoder().encode(type);
  view.setUint32(0, data.length);
  out.set(tag, 4);
  out.set(data, 8);
  view.setUint32(8 + data.length, crc32(tag, data));
  return out;
}

/** Encodes RGBA8 pixels (rows top to bottom, no padding) as a PNG. */
export function encodePng(width: number, height: number, rgba: Uint8Array): Uint8Array<ArrayBuffer> {
  if (rgba.length !== width * height * 4) {
    throw new Error(`a ${width}x${height} frame has ${width * height * 4} bytes, not ${rgba.length}`);
  }
  const header = new Uint8Array(13);
  const hv = new DataView(header.buffer);
  hv.setUint32(0, width);
  hv.setUint32(4, height);
  header.set([8, 6, 0, 0, 0], 8); // 8 bits, RGBA, deflate, adaptive filtering, no interlace
  // Each row is filter type 0 (none) then its pixels.
  const row = width * 4;
  const raw = new Uint8Array(height * (row + 1));
  for (let y = 0; y < height; y++) raw.set(rgba.subarray(y * row, (y + 1) * row), y * (row + 1) + 1);
  const parts = [
    new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", zlibStored(raw)),
    chunk("IEND", new Uint8Array(0)),
  ];
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}
