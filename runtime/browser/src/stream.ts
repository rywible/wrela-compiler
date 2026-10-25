// The command stream, version 12: decoding and sequencing. A line-for-line mirror of
// runtime/abi/src/stream.rs (`decode`, `Sequencer`, `StreamError`), with the same checks in the
// same order and the same messages, so both hosts reject the same batches the same way. The
// format's constants come from abi.gen.ts.

import {
  COMMAND_HEADER_LEN,
  COMPARES,
  HEADER_LEN,
  MAX_ANISOTROPY,
  MAX_COLOR_ATTACHMENTS,
  MAX_LEVELS,
  NONE,
  Opcode,
  ROW_ALIGNMENT,
  SCREEN,
  STREAM_MAGIC,
  STREAM_VERSION,
  TEXTURE_FORMATS,
  TEXTURE_KINDS,
  TEXTURE_WRITABLE,
} from "./abi.gen.ts";
import { UTF8 } from "./wasm.ts";

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

/** A texture's kind (runtime/abi `TextureKind`): WebGPU's dimension name. */
export type TextureKind = (typeof TEXTURE_KINDS)[number];

/** One colour target of a pass: a texture's level and layer, or the screen. */
export type ColorTarget = {
  texture: number;
  level: number;
  layer: number;
  keep: boolean;
  clear: [number, number, number, number];
};

/** A pass's attachments (`BeginPass`). */
export type Pass = {
  colors: ColorTarget[];
  /** It may run as part of the pass before it (`Pass::join`). */
  join: boolean;
  depth: number;
  depthLayer: number;
  keepDepth: boolean;
  clearDepth: number;
};

/** A pass into one colour target, `texture` (or SCREEN), with no depth. */
export function onePass(texture: number, keep: boolean, clear: [number, number, number, number]): Pass {
  return { colors: [{ texture, level: 0, layer: 0, keep, clear }], join: false, depth: NONE, depthLayer: 0, keepDepth: false, clearDepth: 1 };
}

/** Whether a pass draws on the screen. */
export const onScreen = (p: Pass) => p.colors.some((c) => c.texture === SCREEN);

/** Whether pass `next`, beginning right after `ended` ended, may run as part of it, as one
 * render pass (`Pass::joins`, which the vectors check): it may join, on the same targets, kept
 * (not cleared), and not the screen. */
export function joins(next: Pass, ended: Pass): boolean {
  return (
    next.join &&
    !onScreen(next) &&
    next.colors.length === ended.colors.length &&
    next.colors.every((c, i) => {
      const e = ended.colors[i]!;
      return c.keep && c.texture === e.texture && c.level === e.level && c.layer === e.layer;
    }) &&
    next.depth === ended.depth &&
    next.depthLayer === ended.depthLayer &&
    (next.depth === NONE || next.keepDepth)
  );
}

/** A rectangle of texels at one level and layer of a texture (runtime/abi `TexelRect`). */
export type TexelRect = { texture: number; level: number; layer: number; x: number; y: number; width: number; height: number };

export type Command =
  | { op: "CreateBuffer"; handle: number; size: number }
  | { op: "WriteBuffer"; handle: number; offset: number; data: Bytes }
  | { op: "Dispatch"; pipeline: number; groups: [number, number, number]; bindings: Binding[]; uniforms: Bytes }
  | { op: "BeginScreenPass"; clear: [number, number, number, number] }
  | { op: "Draw"; pipeline: number; vertices: number; instances: number; bindings: Binding[]; uniforms: Bytes }
  | { op: "Present" }
  | { op: "DestroyBuffer"; handle: number }
  | { op: "CopyBuffer"; source: number; sourceOffset: number; destination: number; destinationOffset: number; size: number }
  | {
      op: "CreateTexture";
      handle: number;
      width: number;
      height: number;
      format: TextureFormat;
      writable: boolean;
      kind: TextureKind;
      /** A 3D texture's depth, an array's layers (6 of a cube), 1 of a 2D texture. */
      layers: number;
      levels: number;
    }
  | { op: "WriteTexture"; handle: number; level: number; layer: number; x: number; y: number; width: number; height: number; data: Bytes }
  | { op: "DestroyTexture"; handle: number }
  | { op: "CreateSampler"; handle: number; linear: boolean; repeat: boolean; compare: Compare | null; mipLinear: boolean; anisotropy: number }
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
  | { op: "Label"; name: string }
  | { op: "Post"; request: number; url: string; body: Bytes }
  | { op: "HostRequest"; request: number; service: string; body: Bytes }
  | { op: "Prepare"; request: number; path: string }
  | { op: "Unprepare"; path: string }
  | { op: "HandOver"; request: number; path: string }
  | { op: "Timed"; request: number; label: string }
  | { op: "CopyTexture"; source: TexelRect; destination: { texture: number; level: number; layer: number; x: number; y: number } }
  | { op: "CopyTextureToBuffer"; source: TexelRect; buffer: number; offset: number; bytesPerRow: number }
  | { op: "CopyBufferToTexture"; buffer: number; offset: number; bytesPerRow: number; destination: TexelRect }
  | { op: "SetScissor"; x: number; y: number; width: number; height: number }
  | { op: "SetViewport"; rect: [number, number, number, number]; minDepth: number; maxDepth: number }
  | {
      op: "DrawIndexed";
      pipeline: number;
      indices: number;
      index_offset: number;
      index_size: number;
      index_count: number;
      instances: number;
      first_index: number;
      base_vertex: number;
      first_instance: number;
      bindings: Binding[];
      uniforms: Bytes;
    }
  | {
      op: "MultiDrawIndirect" | "MultiDrawIndexedIndirect";
      pipeline: number;
      /** An indexed one's index buffer: its handle, byte offset and size. */
      indices: [number, number, number] | null;
      arguments: number;
      offset: number;
      count: number;
      count_buffer: number;
      count_offset: number;
      bindings: Binding[];
      uniforms: Bytes;
    };

/** The draws: what happens inside a pass, with scissors and viewports. */
export type DrawCommand = Extract<Command, { op: "Draw" | "DrawIndirect" | "DrawIndexed" | "DrawIndexedIndirect" | "MultiDrawIndirect" | "MultiDrawIndexedIndirect" }>;
export const isDraw = (c: Command): c is DrawCommand =>
  c.op === "Draw" || c.op === "DrawIndirect" || c.op === "DrawIndexed" || c.op === "DrawIndexedIndirect" || c.op === "MultiDrawIndirect" || c.op === "MultiDrawIndexedIndirect";
/** The buffer an indirect draw reads its arguments from; null for a direct draw. */
export const argumentsOf = (d: DrawCommand): number | null => (d.op === "Draw" || d.op === "DrawIndexed" ? null : d.arguments);
/** The buffer an indexed draw reads its indices from; null for a draw that isn't indexed. */
export function indicesOf(d: DrawCommand): number | null {
  switch (d.op) {
    case "DrawIndexed":
    case "DrawIndexedIndirect":
      return d.indices;
    case "MultiDrawIndirect":
    case "MultiDrawIndexedIndirect":
      return d.indices === null ? null : d.indices[0];
    default:
      return null;
  }
}

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
  6: "expected 6 words",
  7: "expected 7 words",
  10: "expected 10 words",
  12: "expected 12 words",
};

/** `n` rounded up to a multiple of 4: text and data in a payload are padded to whole words. */
const padded = (n: number) => Math.ceil(n / 4) * 4;

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
        return UTF8.decode(batch.slice(start + from, start + from + n));
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
        exactly(7);
        // Bit 16 of the format word: kernels write it (a storage texture too).
        const writable = (w(3) & TEXTURE_WRITABLE) !== 0;
        const format = TEXTURE_FORMATS[w(3) & ~TEXTURE_WRITABLE];
        if (format === undefined) throw bad("unknown texture format");
        const kind = TEXTURE_KINDS[w(4)];
        if (kind === undefined) throw bad("unknown texture kind");
        const layers = w(5);
        const levels = w(6);
        if (w(1) === 0 || w(2) === 0 || layers === 0) throw bad("a texture's width, height and depth or layers are positive");
        if (levels === 0 || levels > MAX_LEVELS) throw bad("a texture has 1 to 14 mip levels");
        if (writable && !format.storable) throw bad("kernels write only a format the format table says they can");
        if (kind === "2d" && layers !== 1) throw bad("a 2D texture has one layer");
        if (kind === "cube" && (layers !== 6 || w(1) !== w(2))) throw bad("a cube has six square layers");
        if (kind === "3d" && (format.depth || format.block > 1)) throw bad("a 3D texture holds uncompressed colours");
        if (kind === "3d" && levels !== 1) throw bad("a 3D texture has one level");
        cmd = { op: name, handle: w(0), width: w(1), height: w(2), format: format.name, writable, kind, layers, levels };
        break;
      }
      case "WriteTexture": {
        if (words < 8) throw bad("expected at least 8 words");
        if (padded(w(7)) !== len - 32) throw bad("data length doesn't match the payload");
        const data = batch.subarray(start + 32, start + 32 + w(7));
        cmd = { op: name, handle: w(0), level: w(1), layer: w(2), x: w(3), y: w(4), width: w(5), height: w(6), data };
        break;
      }
      case "CreateSampler": {
        exactly(6);
        if (w(1) > 1 || w(2) > 1 || w(4) > 1) throw bad("a sampler's filters and address are 0 or 1");
        if (w(5) === 0 || w(5) > MAX_ANISOTROPY) throw bad("a sampler's anisotropy is 1 to 16");
        if (w(5) > 1 && (w(1) === 0 || w(4) === 0)) throw bad("an anisotropic sampler filters linearly, between levels too");
        let compare: Compare | null = null;
        if (w(3) !== 0) {
          compare = COMPARES[w(3)] ?? null;
          if (compare === null) throw bad("unknown comparison");
        }
        cmd = { op: name, handle: w(0), linear: w(1) === 1, repeat: w(2) === 1, compare, mipLinear: w(4) === 1, anisotropy: w(5) };
        break;
      }
      case "BeginPass": {
        if (words < 2) throw bad("expected at least 2 words");
        const n = w(0);
        if (n > MAX_COLOR_ATTACHMENTS) throw bad("a pass has at most four colour targets");
        if (words !== 2 + 8 * n + 4) throw bad("the colour targets don't fill the payload");
        if (w(1) > 1) throw bad("the join word is 0 or 1");
        const colors: ColorTarget[] = [];
        for (let k = 0; k < n; k++) {
          const c = 2 + 8 * k;
          if (w(c + 3) > 1) throw bad("a load is 0 (clear) or 1 (keep)");
          colors.push({ texture: w(c), level: w(c + 1), layer: w(c + 2), keep: w(c + 3) === 1, clear: [f(c + 4), f(c + 5), f(c + 6), f(c + 7)] });
        }
        if (colors.some((c) => c.texture === SCREEN) && n !== 1) throw bad("a pass on the screen has the screen as its one colour");
        const d = 2 + 8 * n;
        if (w(d + 2) > 1) throw bad("a load is 0 (clear) or 1 (keep)");
        const pass: Pass = { colors, join: w(1) === 1, depth: w(d), depthLayer: w(d + 1), keepDepth: w(d + 2) === 1, clearDepth: f(d + 3) };
        cmd = { op: name, pass };
        break;
      }
      case "CopyTexture": {
        exactly(12);
        cmd = {
          op: name,
          source: { texture: w(0), level: w(1), layer: w(2), x: w(3), y: w(4), width: w(10), height: w(11) },
          destination: { texture: w(5), level: w(6), layer: w(7), x: w(8), y: w(9) },
        };
        break;
      }
      case "CopyTextureToBuffer": {
        exactly(10);
        if (w(8) % ROW_ALIGNMENT !== 0 || w(9) % ROW_ALIGNMENT !== 0 || w(9) === 0) throw bad("the buffer's offset and bytes per row are multiples of 256");
        const source = { texture: w(0), level: w(1), layer: w(2), x: w(3), y: w(4), width: w(5), height: w(6) };
        cmd = { op: name, source, buffer: w(7), offset: w(8), bytesPerRow: w(9) };
        break;
      }
      case "CopyBufferToTexture": {
        exactly(10);
        if (w(1) % 4 !== 0 || w(2) % ROW_ALIGNMENT !== 0 || w(2) === 0) throw bad("the buffer's offset is a multiple of 4, its bytes per row of 256");
        const destination = { texture: w(3), level: w(4), layer: w(5), x: w(6), y: w(7), width: w(8), height: w(9) };
        cmd = { op: name, buffer: w(0), offset: w(1), bytesPerRow: w(2), destination };
        break;
      }
      case "SetScissor": {
        exactly(4);
        cmd = { op: name, x: w(0), y: w(1), width: w(2), height: w(3) };
        break;
      }
      case "SetViewport": {
        exactly(6);
        const rect: [number, number, number, number] = [f(0), f(1), f(2), f(3)];
        const minDepth = f(4);
        const maxDepth = f(5);
        if (!rect.every(Number.isFinite) || rect[2] < 0 || rect[3] < 0) throw bad("a viewport is a finite rectangle of positive size");
        if (!(minDepth >= 0 && minDepth <= 1) || !(maxDepth >= 0 && maxDepth <= 1) || minDepth > maxDepth) throw bad("a viewport's depths are in [0, 1], the least first");
        cmd = { op: name, rect, minDepth, maxDepth };
        break;
      }
      case "DrawIndexed": {
        const [bindings, uniforms] = bound(9);
        if (w(2) % 4 !== 0 || w(3) % 4 !== 0) throw bad("the indices' offset and size must be multiples of 4");
        cmd = {
          op: name,
          pipeline: w(0),
          indices: w(1),
          index_offset: w(2),
          index_size: w(3),
          index_count: w(4),
          instances: w(5),
          first_index: w(6),
          base_vertex: view.getInt32(start + 28, true),
          first_instance: w(8),
          bindings,
          uniforms,
        };
        break;
      }
      case "MultiDrawIndirect":
      case "MultiDrawIndexedIndirect": {
        const indexed = name === "MultiDrawIndexedIndirect";
        const at = indexed ? 4 : 1;
        const [bindings, uniforms] = bound(at + 5);
        if (indexed && (w(2) % 4 !== 0 || w(3) % 4 !== 0)) throw bad("the indices' offset and size must be multiples of 4");
        if (w(at + 1) % 4 !== 0 || w(at + 4) % 4 !== 0) throw bad("the arguments' and the count's offsets must be multiples of 4");
        cmd = {
          op: name,
          pipeline: w(0),
          indices: indexed ? [w(1), w(2), w(3)] : null,
          arguments: w(at),
          offset: w(at + 1),
          count: w(at + 2),
          count_buffer: w(at + 3),
          count_offset: w(at + 4),
          bindings,
          uniforms,
        };
        break;
      }
      case "ReadBuffer": {
        exactly(4);
        if (w(2) % 4 !== 0 || w(3) % 4 !== 0) throw bad("offset and size must be multiples of 4");
        cmd = { op: name, request: w(0), handle: w(1), offset: w(2), size: w(3) };
        break;
      }
      case "StorageRead":
      case "Fetch":
      case "Prepare":
      case "HandOver":
      case "Timed": {
        if (words < 2) throw bad("expected at least 2 words");
        const n = w(1);
        if (padded(n) !== len - 8) throw bad("the text's length doesn't match the payload");
        const t = text(8, n, "text");
        if (name === "Fetch") cmd = { op: name, request: w(0), url: t };
        else if (name === "Timed") cmd = { op: name, request: w(0), label: t };
        else cmd = { op: name, request: w(0), path: t };
        break;
      }
      case "Label":
      case "Unprepare": {
        if (words < 1) throw bad("expected at least 1 word");
        const n = w(0);
        if (padded(n) !== len - 4) throw bad("the text's length doesn't match the payload");
        const t = text(4, n, "text");
        cmd = name === "Label" ? { op: name, name: t } : { op: name, path: t };
        break;
      }
      case "StorageWrite": {
        const [path, data] = twoRuns("path", "the path's and data's lengths don't match the payload");
        cmd = { op: name, request: w(0), path, data };
        break;
      }
      case "HostRequest": {
        const [service, body] = twoRuns("service", "the service and body lengths do not match the payload");
        cmd = { op: name, request: w(0), service, body };
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
        const screen = cmd.op === "BeginScreenPass" || onScreen(cmd.pass);
        this.#at = screen ? "screen pass" : "pass";
        return;
      }
      case "Draw":
      case "DrawIndirect":
      case "DrawIndexed":
      case "DrawIndexedIndirect":
      case "MultiDrawIndirect":
      case "MultiDrawIndexedIndirect":
        if (this.#at === "outside") throw err("a draw must come inside a pass");
        return;
      case "SetScissor":
      case "SetViewport":
        if (this.#at === "outside") throw err("a scissor or a viewport is set inside a pass");
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
        if (this.#at !== "outside") throw err("only draws, scissors and viewports can happen inside a pass");
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
