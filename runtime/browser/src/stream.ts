// The command stream, version 4: decoding and sequencing. A line-for-line mirror of
// runtime/abi/src/stream.rs (`decode`, `Sequencer`, `StreamError`), with the same checks in the
// same order and the same messages, so both hosts reject the same batches the same way. The
// format's constants come from abi.gen.ts.

import {
  COMMAND_HEADER_LEN,
  COMPARES,
  HEADER_LEN,
  Opcode,
  SCREEN,
  STREAM_MAGIC,
  STREAM_VERSION,
  TEXTURE_FORMATS,
} from "./abi.gen.ts";

export type OpcodeName = keyof typeof Opcode;

/** Bytes in the program's memory, which is shared. (WebGPU's typings ask for an unshared
 * ArrayBuffer, but `writeBuffer` and `writeTexture` take a view of shared memory: D-097,
 * measured on Chrome stable, 2026-10-03.) A command's bytes are views of the batch, so whatever
 * keeps them past the batch copies them. */
export type Bytes = Uint8Array<ArrayBuffer>;

/** A texture's format: WebGPU's name for it. */
export type TextureFormat = (typeof TEXTURE_FORMATS)[number]["name"];
/** A comparison sampler's test: WebGPU's name for it. */
export type Compare = Exclude<(typeof COMPARES)[number], null>;

/** One binding: a buffer's range, or a texture or sampler (offset and size 0). */
export type Binding = { handle: number; offset: number; size: number };

/** A pass's attachments (`BeginPass`). */
export type Pass = {
  color: number;
  keepColor: boolean;
  clear: [number, number, number, number];
  depth: number;
  keepDepth: boolean;
  clearDepth: number;
};

export type Command =
  | { op: "CreateBuffer"; handle: number; size: number }
  | { op: "WriteBuffer"; handle: number; offset: number; data: Bytes }
  | { op: "Dispatch"; pipeline: number; groups: [number, number, number]; bindings: Binding[]; uniforms: Bytes }
  | { op: "BeginScreenPass"; clear: [number, number, number, number] }
  | { op: "Draw"; pipeline: number; vertices: number; instances: number; bindings: Binding[]; uniforms: Bytes }
  | { op: "Present" }
  | { op: "DestroyBuffer"; handle: number }
  | { op: "CopyBuffer"; source: number; sourceOffset: number; destination: number; destinationOffset: number; size: number }
  | { op: "CreateTexture"; handle: number; width: number; height: number; format: TextureFormat }
  | { op: "WriteTexture"; handle: number; x: number; y: number; width: number; height: number; data: Bytes }
  | { op: "DestroyTexture"; handle: number }
  | { op: "CreateSampler"; handle: number; linear: boolean; repeat: boolean; compare: Compare | null }
  | { op: "DestroySampler"; handle: number }
  | { op: "BeginPass"; pass: Pass }
  | { op: "EndPass" }
  | { op: "DispatchIndirect"; pipeline: number; arguments: number; offset: number; bindings: Binding[]; uniforms: Bytes }
  | { op: "DrawIndirect"; pipeline: number; arguments: number; offset: number; bindings: Binding[]; uniforms: Bytes }
  | {
      op: "DrawIndexedIndirect";
      pipeline: number;
      indices: number;
      index_offset: number;
      index_size: number;
      arguments: number;
      offset: number;
      bindings: Binding[];
      uniforms: Bytes;
    }
  | { op: "ReadBuffer"; request: number; handle: number; offset: number; size: number }
  | { op: "StorageRead"; request: number; path: string }
  | { op: "StorageWrite"; request: number; path: string; data: Bytes }
  | { op: "Fetch"; request: number; url: string }
  | { op: "Log"; text: string }
  | { op: "Post"; request: number; url: string; body: Bytes };

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

/** What `decode` says when a command doesn't have exactly the number of words it needs, by
 * that number. Only the numbers that some command needs are here. */
const EXPECTED: Readonly<Record<number, string>> = {
  0: "expected no payload",
  1: "expected 1 word",
  2: "expected 2 words",
  4: "expected 4 words",
  5: "expected 5 words",
  9: "expected 9 words",
};

/** `n` rounded up to a multiple of 4: text and data in a payload are padded to whole words. */
const padded = (n: number) => Math.ceil(n / 4) * 4;

const utf8 = new TextDecoder("utf-8", { fatal: true });

/**
 * Decodes one batch: its header, then every command. Byte payloads are views into `batch`, so
 * copy any you keep past the call that submitted it. It doesn't check pass sequencing (that
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
    const f = (i: number) => view.getFloat32(start + i * 4, true);
    const exactly = (n: number) => {
      if (words !== n) throw bad(EXPECTED[n]!);
    };
    const text = (from: number, n: number, what: string) => {
      try {
        // A copy: TextDecoder doesn't read shared memory.
        return utf8.decode(batch.slice(start + from, start + from + n));
      } catch {
        throw bad(`the ${what} isn't UTF-8`);
      }
    };
    // The bindings and uniforms that end a dispatch's or draw's payload (words from `from`).
    const bound = (from: number): [Binding[], Bytes] => {
      if (words < from + 2) throw bad("payload too short");
      const n = w(from);
      if (words < from + 1 + 3 * n + 1) throw bad("binding list runs past the payload");
      const bindings: Binding[] = [];
      for (let k = 0; k < n; k++) {
        const i = from + 1 + 3 * k;
        bindings.push({ handle: w(i), offset: w(i + 1), size: w(i + 2) });
      }
      const ulen = w(from + 1 + 3 * n);
      const ustart = (from + 2 + 3 * n) * 4;
      if (ulen !== len - ustart) throw bad("uniform length doesn't match the payload");
      return [bindings, batch.subarray(start + ustart, start + len)];
    };
    // A text, then bytes, their lengths in words 1 and 2 (`mismatch` if they don't fill the
    // payload). `what` names the text if it isn't UTF-8.
    const twoRuns = (what: string, mismatch: string): [string, Bytes] => {
      if (words < 3) throw bad("expected at least 3 words");
      const n = w(1);
      const m = w(2);
      if (padded(n) + padded(m) !== len - 12) throw bad(mismatch);
      const from = start + 12 + padded(n);
      return [text(12, n, what), batch.subarray(from, from + m)];
    };
    let cmd: Command;
    switch (name) {
      case "CreateBuffer": {
        exactly(2);
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
      case "Dispatch": {
        const [bindings, uniforms] = bound(4);
        cmd = { op: name, pipeline: w(0), groups: [w(1), w(2), w(3)], bindings, uniforms };
        break;
      }
      case "Draw": {
        const [bindings, uniforms] = bound(3);
        cmd = { op: name, pipeline: w(0), vertices: w(1), instances: w(2), bindings, uniforms };
        break;
      }
      case "DispatchIndirect":
      case "DrawIndirect": {
        const [bindings, uniforms] = bound(3);
        if (w(2) % 4 !== 0) throw bad("the arguments' offset must be a multiple of 4");
        cmd = { op: name, pipeline: w(0), arguments: w(1), offset: w(2), bindings, uniforms };
        break;
      }
      case "DrawIndexedIndirect": {
        const [bindings, uniforms] = bound(6);
        if (w(2) % 4 !== 0 || w(3) % 4 !== 0) throw bad("the indices' offset and size must be multiples of 4");
        if (w(5) % 4 !== 0) throw bad("the arguments' offset must be a multiple of 4");
        cmd = {
          op: name,
          pipeline: w(0),
          indices: w(1),
          index_offset: w(2),
          index_size: w(3),
          arguments: w(4),
          offset: w(5),
          bindings,
          uniforms,
        };
        break;
      }
      case "BeginScreenPass": {
        exactly(4);
        cmd = { op: name, clear: [f(0), f(1), f(2), f(3)] };
        break;
      }
      case "Present":
      case "EndPass": {
        exactly(0);
        cmd = { op: name };
        break;
      }
      case "DestroyBuffer":
      case "DestroyTexture":
      case "DestroySampler": {
        exactly(1);
        cmd = { op: name, handle: w(0) };
        break;
      }
      case "CopyBuffer": {
        exactly(5);
        if ([w(1), w(3), w(4)].some((x) => x % 4 !== 0)) throw bad("offsets and size must be multiples of 4");
        cmd = { op: name, source: w(0), sourceOffset: w(1), destination: w(2), destinationOffset: w(3), size: w(4) };
        break;
      }
      case "CreateTexture": {
        exactly(4);
        const format = TEXTURE_FORMATS[w(3)];
        if (format === undefined) throw bad("unknown texture format");
        if (w(1) === 0 || w(2) === 0) throw bad("a texture's width and height are positive");
        cmd = { op: name, handle: w(0), width: w(1), height: w(2), format: format.name };
        break;
      }
      case "WriteTexture": {
        if (words < 6) throw bad("expected at least 6 words");
        if (w(5) !== len - 24) throw bad("data length doesn't match the payload");
        const data = batch.subarray(start + 24, start + len);
        cmd = { op: name, handle: w(0), x: w(1), y: w(2), width: w(3), height: w(4), data };
        break;
      }
      case "CreateSampler": {
        exactly(4);
        if (w(1) > 1 || w(2) > 1) throw bad("a sampler's filter and address are 0 or 1");
        let compare: Compare | null = null;
        if (w(3) !== 0) {
          compare = COMPARES[w(3)] ?? null;
          if (compare === null) throw bad("unknown comparison");
        }
        cmd = { op: name, handle: w(0), linear: w(1) === 1, repeat: w(2) === 1, compare };
        break;
      }
      case "BeginPass": {
        exactly(9);
        if (w(1) > 1 || w(7) > 1) throw bad("a load is 0 (clear) or 1 (keep)");
        const pass: Pass = {
          color: w(0),
          keepColor: w(1) === 1,
          clear: [f(2), f(3), f(4), f(5)],
          depth: w(6),
          keepDepth: w(7) === 1,
          clearDepth: f(8),
        };
        cmd = { op: name, pass };
        break;
      }
      case "ReadBuffer": {
        exactly(4);
        if (w(2) % 4 !== 0 || w(3) % 4 !== 0) throw bad("offset and size must be multiples of 4");
        cmd = { op: name, request: w(0), handle: w(1), offset: w(2), size: w(3) };
        break;
      }
      case "StorageRead":
      case "Fetch": {
        if (words < 2) throw bad("expected at least 2 words");
        const n = w(1);
        if (padded(n) !== len - 8) throw bad("the text's length doesn't match the payload");
        const t = text(8, n, "text");
        cmd = name === "Fetch" ? { op: name, request: w(0), url: t } : { op: name, request: w(0), path: t };
        break;
      }
      case "Log": {
        if (words < 1) throw bad("expected at least 1 word");
        const n = w(0);
        if (padded(n) !== len - 4) throw bad("the text's length doesn't match the payload");
        cmd = { op: name, text: text(4, n, "text") };
        break;
      }
      case "StorageWrite": {
        const [path, data] = twoRuns("path", "the path's and data's lengths don't match the payload");
        cmd = { op: name, request: w(0), path, data };
        break;
      }
      case "Post": {
        const [url, body] = twoRuns("URL", "the URL's and body's lengths don't match the payload");
        cmd = { op: name, request: w(0), url, body };
        break;
      }
    }
    out.push(cmd);
    at = start + len;
  }
  return out;
}

/** Where commands can be, between `Sequencer` steps. */
type Place = "outside" | "screen pass" | "pass";

/** Checks that commands come in a valid order across batches. */
export class Sequencer {
  #at: Place = "outside";

  /** A copy, to go back to when a command it passed is rejected after it. */
  copy(): Sequencer {
    const s = new Sequencer();
    s.#at = this.#at;
    return s;
  }

  step(cmd: Command): void {
    const err = (why: string) => new StreamError("Sequence", `${cmd.op} out of sequence: ${why}`);
    switch (cmd.op) {
      case "BeginScreenPass":
      case "BeginPass": {
        if (this.#at !== "outside") throw err("a pass is already open");
        const screen = cmd.op === "BeginScreenPass" || cmd.pass.color === SCREEN;
        this.#at = screen ? "screen pass" : "pass";
        return;
      }
      case "Draw":
      case "DrawIndirect":
      case "DrawIndexedIndirect":
        if (this.#at === "outside") throw err("a draw must come inside a pass");
        return;
      case "Present":
        if (this.#at !== "screen pass") throw err("Present must close a pass on the screen");
        this.#at = "outside";
        return;
      case "EndPass":
        if (this.#at !== "pass") throw err("EndPass must close a pass that isn't on the screen");
        this.#at = "outside";
        return;
      default:
        if (this.#at !== "outside") throw err("only draws can happen inside a pass");
        return;
    }
  }

  /** Checks the end of a call: a host calls this after each call into the program returns. */
  endFrame(): void {
    if (this.#at !== "outside") {
      throw new StreamError("UnclosedPass", "a call returned with a pass still open (no Present or EndPass)");
    }
  }
}
