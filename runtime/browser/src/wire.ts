// The bytes of a host service's request and reply, as Wrela's `std::serialize::Writer` writes
// them: little-endian words, an f32 as its bits, a string as its length in bytes then its UTF-8.

const ENCODER = new TextEncoder();
const DECODER = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

/** Reads a request's body from the start; each read past its end throws. */
export class Reader {
  #view: DataView;
  #at = 0;
  constructor(readonly bytes: Uint8Array) {
    this.#view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }
  #need(n: number): number {
    if (this.#at + n > this.bytes.length) throw new Error("the request is shorter than it says");
    const at = this.#at;
    this.#at += n;
    return at;
  }
  u8(): number {
    return this.bytes[this.#need(1)]!;
  }
  u32(): number {
    return this.#view.getUint32(this.#need(4), true);
  }
  f32(): number {
    return this.#view.getFloat32(this.#need(4), true);
  }
  str(): string {
    const n = this.u32();
    const at = this.#need(n);
    return DECODER.decode(this.bytes.subarray(at, at + n));
  }
  /** What's left, as text. */
  rest(): string {
    return DECODER.decode(this.bytes.subarray(this.#need(this.bytes.length - this.#at)));
  }
}

/** Builds a reply. */
export class Writer {
  #parts: Uint8Array[] = [];
  u8(n: number): this {
    this.#parts.push(Uint8Array.of(n));
    return this;
  }
  u32(n: number): this {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, n >>> 0, true);
    this.#parts.push(b);
    return this;
  }
  f32(x: number): this {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setFloat32(0, x, true);
    this.#parts.push(b);
    return this;
  }
  str(s: string): this {
    const b = ENCODER.encode(s);
    this.u32(b.length);
    this.#parts.push(b);
    return this;
  }
  raw(b: Uint8Array): this {
    this.#parts.push(b);
    return this;
  }
  bytes(): Uint8Array<ArrayBuffer> {
    const out = new Uint8Array(this.#parts.reduce((n, p) => n + p.length, 0));
    let at = 0;
    for (const p of this.#parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  }
}
