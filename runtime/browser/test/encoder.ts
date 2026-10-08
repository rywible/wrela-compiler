// A test-only command-stream encoder, mirroring `wrela_abi::stream::Encoder`. The golden test in
// stream.test.ts pins its bytes to the same expected bytes as the Rust crate's golden test.

import { COMPARES, HEADER_LEN, Opcode, STREAM_MAGIC, STREAM_VERSION, TEXTURE_FORMATS } from "../src/abi.gen.ts";
import type { Binding, Compare, Pass, TextureFormat } from "../src/stream.ts";

export const words = (ws: number[]): Uint8Array<ArrayBuffer> => {
  const out = new Uint8Array(ws.length * 4);
  const view = new DataView(out.buffer);
  ws.forEach((w, i) => view.setUint32(i * 4, w >>> 0, true));
  return out;
};

const f32bits = (f: number) => {
  const view = new DataView(new ArrayBuffer(4));
  view.setFloat32(0, f, true);
  return view.getUint32(0, true);
};

/** A binding: a buffer's handle (its first 64 bytes unless given), or `[handle, offset, size]`. */
export type B = number | [number, number, number];
const binding = (b: B): Binding => (typeof b === "number" ? { handle: b, offset: 0, size: 64 } : { handle: b[0], offset: b[1], size: b[2] });

/** Bytes padded with zeros to a multiple of 4. */
const pad = (bytes: Uint8Array) => {
  const out = new Uint8Array(Math.ceil(bytes.length / 4) * 4);
  out.set(bytes);
  return out;
};
const utf8 = (s: string) => new TextEncoder().encode(s);

export class Encoder {
  #body: number[] = [];

  #command(op: number, payload: number[], bytes: Uint8Array = new Uint8Array(0)): this {
    if (bytes.length % 4 !== 0) throw new Error("payload bytes must be a multiple of 4");
    this.#body.push(...words([op, payload.length * 4 + bytes.length, ...payload]), ...bytes);
    return this;
  }

  /** The words of a binding list and a uniform byte count. */
  static #bound(bindings: B[], uniforms: Uint8Array): number[] {
    return [bindings.length, ...bindings.map(binding).flatMap((b) => [b.handle, b.offset, b.size]), uniforms.length];
  }

  createBuffer(handle: number, size: number): this {
    return this.#command(Opcode.CreateBuffer, [handle, size]);
  }

  writeBuffer(handle: number, offset: number, data: Uint8Array): this {
    return this.#command(Opcode.WriteBuffer, [handle, offset, data.length], data);
  }

  dispatch(pipeline: number, groups: [number, number, number], bindings: B[], uniforms: Uint8Array): this {
    return this.#command(Opcode.Dispatch, [pipeline, ...groups, ...Encoder.#bound(bindings, uniforms)], uniforms);
  }

  beginScreenPass(clear: [number, number, number, number]): this {
    return this.#command(Opcode.BeginScreenPass, clear.map(f32bits));
  }

  draw(pipeline: number, vertices: number, instances: number, bindings: B[], uniforms: Uint8Array): this {
    return this.#command(Opcode.Draw, [pipeline, vertices, instances, ...Encoder.#bound(bindings, uniforms)], uniforms);
  }

  present(): this {
    return this.#command(Opcode.Present, []);
  }

  destroyBuffer(handle: number): this {
    return this.#command(Opcode.DestroyBuffer, [handle]);
  }

  copyBuffer(source: number, sourceOffset: number, destination: number, destinationOffset: number, size: number): this {
    return this.#command(Opcode.CopyBuffer, [source, sourceOffset, destination, destinationOffset, size]);
  }

  createTexture(handle: number, width: number, height: number, format: TextureFormat): this {
    const f = TEXTURE_FORMATS.findIndex((t) => t.name === format);
    return this.#command(Opcode.CreateTexture, [handle, width, height, f]);
  }

  writeTexture(handle: number, origin: [number, number], size: [number, number], data: Uint8Array): this {
    return this.#command(Opcode.WriteTexture, [handle, ...origin, ...size, data.length], data);
  }

  destroyTexture(handle: number): this {
    return this.#command(Opcode.DestroyTexture, [handle]);
  }

  createSampler(handle: number, linear: boolean, repeat: boolean, compare: Compare | null): this {
    const c = compare === null ? 0 : COMPARES.indexOf(compare);
    return this.#command(Opcode.CreateSampler, [handle, Number(linear), Number(repeat), c]);
  }

  destroySampler(handle: number): this {
    return this.#command(Opcode.DestroySampler, [handle]);
  }

  beginPass(p: Pass): this {
    const ws = [p.color, Number(p.keepColor) | (Number(p.join) << 1), ...p.clear.map(f32bits), p.depth, Number(p.keepDepth), f32bits(p.clearDepth)];
    return this.#command(Opcode.BeginPass, ws);
  }

  endPass(): this {
    return this.#command(Opcode.EndPass, []);
  }

  dispatchIndirect(pipeline: number, args: number, offset: number, bindings: B[], uniforms: Uint8Array): this {
    return this.#command(Opcode.DispatchIndirect, [pipeline, args, offset, ...Encoder.#bound(bindings, uniforms)], uniforms);
  }

  drawIndirect(pipeline: number, args: number, offset: number, bindings: B[], uniforms: Uint8Array): this {
    return this.#command(Opcode.DrawIndirect, [pipeline, args, offset, ...Encoder.#bound(bindings, uniforms)], uniforms);
  }

  /** `indices`: the index buffer's handle, offset and size in bytes. */
  drawIndexedIndirect(pipeline: number, indices: [number, number, number], args: number, offset: number, bindings: B[], uniforms: Uint8Array): this {
    return this.#command(Opcode.DrawIndexedIndirect, [pipeline, ...indices, args, offset, ...Encoder.#bound(bindings, uniforms)], uniforms);
  }

  readBuffer(request: number, handle: number, offset: number, size: number): this {
    return this.#command(Opcode.ReadBuffer, [request, handle, offset, size]);
  }

  storageRead(request: number, path: string): this {
    const p = utf8(path);
    return this.#command(Opcode.StorageRead, [request, p.length], pad(p));
  }

  storageWrite(request: number, path: string, data: Uint8Array): this {
    const p = utf8(path);
    const bytes = new Uint8Array([...pad(p), ...pad(data)]);
    return this.#command(Opcode.StorageWrite, [request, p.length, data.length], bytes);
  }

  fetch(request: number, url: string): this {
    const u = utf8(url);
    return this.#command(Opcode.Fetch, [request, u.length], pad(u));
  }

  post(request: number, url: string, body: Uint8Array): this {
    const u = utf8(url);
    const bytes = new Uint8Array([...pad(u), ...pad(body)]);
    return this.#command(Opcode.Post, [request, u.length, body.length], bytes);
  }

  log(text: string): this {
    const t = utf8(text);
    return this.#command(Opcode.Log, [t.length], pad(t));
  }

  label(name: string): this {
    const t = utf8(name);
    return this.#command(Opcode.Label, [t.length], pad(t));
  }

  finish(): Uint8Array<ArrayBuffer> {
    const out = new Uint8Array(HEADER_LEN + this.#body.length);
    out.set(words([STREAM_MAGIC, STREAM_VERSION, this.#body.length]));
    out.set(this.#body, HEADER_LEN);
    return out;
  }
}
