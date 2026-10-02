// A test-only command-stream encoder, mirroring `wrela_abi::stream::Encoder`. The golden test in
// stream.test.ts pins its bytes to the same expected bytes as the Rust crate's golden test.

import { HEADER_LEN, Opcode, STREAM_MAGIC, STREAM_VERSION } from "../src/abi.gen.ts";

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

export class Encoder {
  #body: number[] = [];

  #command(op: number, payload: number[], bytes: Uint8Array = new Uint8Array(0)): this {
    if (bytes.length % 4 !== 0) throw new Error("payload bytes must be a multiple of 4");
    this.#body.push(...words([op, payload.length * 4 + bytes.length, ...payload]), ...bytes);
    return this;
  }

  createBuffer(handle: number, size: number): this {
    return this.#command(Opcode.CreateBuffer, [handle, size]);
  }

  writeBuffer(handle: number, offset: number, data: Uint8Array): this {
    return this.#command(Opcode.WriteBuffer, [handle, offset, data.length], data);
  }

  dispatch(pipeline: number, groups: [number, number, number], buffers: number[], uniforms: Uint8Array): this {
    return this.#command(Opcode.Dispatch, [pipeline, ...groups, buffers.length, ...buffers, uniforms.length], uniforms);
  }

  beginScreenPass(clear: [number, number, number, number]): this {
    return this.#command(Opcode.BeginScreenPass, clear.map(f32bits));
  }

  draw(pipeline: number, vertices: number, instances: number, buffers: number[], uniforms: Uint8Array): this {
    return this.#command(Opcode.Draw, [pipeline, vertices, instances, buffers.length, ...buffers, uniforms.length], uniforms);
  }

  present(): this {
    return this.#command(Opcode.Present, []);
  }

  finish(): Uint8Array<ArrayBuffer> {
    const out = new Uint8Array(HEADER_LEN + this.#body.length);
    out.set(words([STREAM_MAGIC, STREAM_VERSION, this.#body.length]));
    out.set(this.#body, HEADER_LEN);
    return out;
  }
}
