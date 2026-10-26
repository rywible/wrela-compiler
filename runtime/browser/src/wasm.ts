// Just enough of the WASM binary format to read a custom section's encodings (lines.ts).

/** Strict UTF-8 that keeps a leading byte-order mark, as Rust's `str::from_utf8` does. */
export const UTF8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/** Reads WASM's encodings in order; `eof` is the message when the bytes run out. */
export class ByteReader {
  at = 0;
  constructor(
    readonly bytes: Uint8Array,
    readonly eof = "unexpected end of the module",
  ) {}

  byte(): number {
    const b = this.bytes[this.at++];
    if (b === undefined) throw new Error(this.eof);
    return b;
  }

  /** An unsigned LEB128 (up to 64 bits; precision past 2^53 doesn't matter for skipping). */
  uleb(): number {
    let result = 0;
    let scale = 1;
    for (;;) {
      const b = this.byte();
      result += (b & 0x7f) * scale;
      if ((b & 0x80) === 0) return result;
      scale *= 128;
    }
  }

  /** An unsigned LEB128 that fits in 32 bits, in at most 5 bytes. */
  u32(): number {
    let x = 0;
    for (let shift = 0; shift < 35; shift += 7) {
      const b = this.byte();
      x += (b & 0x7f) * 2 ** shift;
      if ((b & 0x80) === 0) {
        if (x > 0xffffffff) throw new Error("a number is too large");
        return x;
      }
    }
    throw new Error("a number is too long");
  }

  /** The next `n` bytes. */
  take(n: number): Uint8Array {
    if (this.at + n > this.bytes.length) throw new Error(this.eof);
    const out = this.bytes.subarray(this.at, this.at + n);
    this.at += n;
    return out;
  }

  /** A length-prefixed UTF-8 string. */
  name(): string {
    return UTF8.decode(this.take(this.u32()));
  }
}
