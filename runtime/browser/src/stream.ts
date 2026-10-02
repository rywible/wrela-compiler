// The command stream, version 1: decoding and sequencing. A line-for-line mirror of
// runtime/abi/src/stream.rs (`decode`, `Sequencer`, `StreamError`), with the same checks in the
// same order and the same messages, so both hosts reject the same batches the same way. The
// format's constants come from abi.gen.ts.

import { COMMAND_HEADER_LEN, HEADER_LEN, Opcode, STREAM_MAGIC, STREAM_VERSION } from "./abi.gen.ts";

export type OpcodeName = keyof typeof Opcode;

/** Bytes in the program's memory (an unshared ArrayBuffer, as WebGPU's typings require). */
export type Bytes = Uint8Array<ArrayBuffer>;

export type Command =
  | { op: "CreateBuffer"; handle: number; size: number }
  | { op: "WriteBuffer"; handle: number; offset: number; data: Bytes }
  | { op: "Dispatch"; pipeline: number; groups: [number, number, number]; buffers: number[]; uniforms: Bytes }
  | { op: "BeginScreenPass"; clear: [number, number, number, number] }
  | { op: "Draw"; pipeline: number; vertices: number; instances: number; buffers: number[]; uniforms: Bytes }
  | { op: "Present" };

const NAMES = new Map<number, OpcodeName>(
  (Object.entries(Opcode) as [OpcodeName, number][]).map(([name, value]) => [value, name]),
);

/** What's wrong with a batch. `kind` names the Rust `StreamError` variant. */
export class StreamError extends Error {
  constructor(
    readonly kind:
      | "TooShort"
      | "BadMagic"
      | "WrongVersion"
      | "BodyLength"
      | "UnknownOpcode"
      | "BadPayload"
      | "Sequence"
      | "UnclosedPass",
    message: string,
  ) {
    super(message);
    this.name = "StreamError";
  }
}

const tooShort = (needed: number, got: number) =>
  new StreamError("TooShort", `batch too short: needs ${needed} bytes, has ${got}`);

/**
 * Decodes one batch: its header, then every command. Byte payloads are views into `batch`, so
 * copy any you keep past the call that submitted it. It doesn't check frame sequencing (that
 * spans batches: see `Sequencer`).
 */
export function decode(batch: Bytes): Command[] {
  const view = new DataView(batch.buffer, batch.byteOffset, batch.byteLength);
  const word = (at: number) => view.getUint32(at, true);
  if (batch.length < HEADER_LEN) {
    throw tooShort(HEADER_LEN, batch.length);
  }
  if (word(0) !== STREAM_MAGIC) {
    const magic = `[${Array.from(batch.subarray(0, 4)).join(", ")}]`;
    throw new StreamError("BadMagic", `not a command batch: magic is ${magic}, not \`WRCS\``);
  }
  const version = word(4);
  if (version !== STREAM_VERSION) {
    throw new StreamError(
      "WrongVersion",
      `command stream version ${version}, but this host reads version ${STREAM_VERSION}`,
    );
  }
  const bodyLen = word(8);
  if (bodyLen !== batch.length - HEADER_LEN) {
    throw new StreamError(
      "BodyLength",
      `batch declares ${bodyLen} bytes of commands but has ${batch.length - HEADER_LEN}`,
    );
  }
  const out: Command[] = [];
  let at = HEADER_LEN;
  while (at < batch.length) {
    if (batch.length - at < COMMAND_HEADER_LEN) {
      throw tooShort(at + COMMAND_HEADER_LEN, batch.length);
    }
    const op = word(at);
    const len = word(at + 4);
    const name = NAMES.get(op);
    if (name === undefined) {
      throw new StreamError("UnknownOpcode", `unknown opcode ${op} at byte ${at}`);
    }
    const start = at + COMMAND_HEADER_LEN;
    const bad = (why: string) => new StreamError("BadPayload", `malformed ${name} at byte ${at}: ${why}`);
    if (len % 4 !== 0) {
      throw bad("payload length isn't a multiple of 4");
    }
    if (batch.length - start < len) {
      throw bad("payload runs past the end of the batch");
    }
    const words = len / 4;
    const w = (i: number) => word(start + i * 4);
    let cmd: Command;
    switch (name) {
      case "CreateBuffer": {
        if (words !== 2) throw bad("expected 2 words");
        const size = w(1);
        if (size === 0 || size % 4 !== 0) throw bad("size must be a positive multiple of 4");
        cmd = { op: name, handle: w(0), size };
        break;
      }
      case "WriteBuffer": {
        if (words < 3) throw bad("expected at least 3 words");
        const offset = w(1);
        const n = w(2);
        if (offset % 4 !== 0 || n % 4 !== 0) throw bad("offset and length must be multiples of 4");
        if (n !== len - 12) throw bad("data length doesn't match the payload");
        cmd = { op: name, handle: w(0), offset, data: batch.subarray(start + 12, start + len) };
        break;
      }
      case "Dispatch":
      case "Draw": {
        const fixed = name === "Dispatch" ? 4 : 3;
        if (words < fixed + 2) throw bad("payload too short");
        const nbuf = w(fixed);
        if (words < fixed + 1 + nbuf + 1) throw bad("buffer list runs past the payload");
        const buffers: number[] = [];
        for (let i = 0; i < nbuf; i++) buffers.push(w(fixed + 1 + i));
        const ulen = w(fixed + 1 + nbuf);
        const ustart = (fixed + 2 + nbuf) * 4;
        if (ulen !== len - ustart) throw bad("uniform length doesn't match the payload");
        const uniforms = batch.subarray(start + ustart, start + len);
        cmd =
          name === "Dispatch"
            ? { op: name, pipeline: w(0), groups: [w(1), w(2), w(3)], buffers, uniforms }
            : { op: name, pipeline: w(0), vertices: w(1), instances: w(2), buffers, uniforms };
        break;
      }
      case "BeginScreenPass": {
        if (words !== 4) throw bad("expected 4 words");
        const f = (i: number) => view.getFloat32(start + i * 4, true);
        cmd = { op: name, clear: [f(0), f(1), f(2), f(3)] };
        break;
      }
      case "Present": {
        if (words !== 0) throw bad("expected no payload");
        cmd = { op: name };
        break;
      }
    }
    out.push(cmd);
    at = start + len;
  }
  return out;
}

/** Checks that commands come in a valid order across batches. */
export class Sequencer {
  #inPass = false;

  get inPass(): boolean {
    return this.#inPass;
  }

  step(cmd: Command): void {
    const err = (why: string) => new StreamError("Sequence", `${cmd.op} out of sequence: ${why}`);
    switch (cmd.op) {
      case "BeginScreenPass":
        if (this.#inPass) throw err("a screen pass is already open");
        this.#inPass = true;
        return;
      case "Draw":
        if (!this.#inPass) throw err("a draw must come after BeginScreenPass");
        return;
      case "Present":
        if (!this.#inPass) throw err("Present must close a screen pass");
        this.#inPass = false;
        return;
      case "Dispatch":
      case "CreateBuffer":
      case "WriteBuffer":
        if (this.#inPass) throw err("only draws can happen inside a screen pass");
        return;
    }
  }

  /** Checks the end of a frame: a host calls this after each call of `frame` returns. */
  endFrame(): void {
    if (this.#inPass) {
      throw new StreamError("UnclosedPass", "frame returned with a screen pass still open (no Present)");
    }
  }
}
