// src/entry.ts
import module from "./game.wasm";

// src/cloudflare.ts
import { DurableObject } from "cloudflare:workers";

// ../browser/src/abi.gen.ts
var STREAM_VERSION = 13;
var STREAM_MAGIC = 1396920919;
var HEADER_LEN = 12;
var COMMAND_HEADER_LEN = 8;
var SCREEN = 4294967294;
var Opcode = {
  CreateBuffer: 1,
  WriteBuffer: 2,
  Dispatch: 3,
  BeginScreenPass: 4,
  Draw: 5,
  Present: 6,
  DestroyBuffer: 7,
  CopyBuffer: 8,
  CreateTexture: 9,
  WriteTexture: 10,
  DestroyTexture: 11,
  CreateSampler: 12,
  DestroySampler: 13,
  BeginPass: 14,
  EndPass: 15,
  DispatchIndirect: 16,
  DrawIndirect: 17,
  ReadBuffer: 18,
  StorageRead: 19,
  StorageWrite: 20,
  Fetch: 21,
  Post: 23,
  DrawIndexedIndirect: 24,
  Label: 25,
  Prepare: 26,
  Unprepare: 27,
  HandOver: 28,
  HostRequest: 29,
  Timed: 30,
  CopyTexture: 31,
  CopyTextureToBuffer: 32,
  CopyBufferToTexture: 33,
  SetScissor: 34,
  SetViewport: 35,
  DrawIndexed: 36,
  MultiDrawIndirect: 37,
  MultiDrawIndexedIndirect: 38,
  ConfigureScreen: 39
};
var TEXTURE_FORMATS = [
  { name: "rgba8unorm", bytes: 4, block: 1, channels: 4, depth: false, storable: true, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 1, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rgba16float", bytes: 8, block: 1, channels: 4, depth: false, storable: true, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 2, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "depth32float", bytes: 4, block: 1, channels: 1, depth: true, storable: false, filterable: false, renderable: true, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "r16float", bytes: 2, block: 1, channels: 1, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 2, target_align: 2, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rg16float", bytes: 4, block: 1, channels: 2, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 4, target_align: 2, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "r32float", bytes: 4, block: 1, channels: 1, depth: false, storable: true, filterable: false, renderable: true, blendable: true, uint: false, target_cost: 4, target_align: 4, needs: { filter: "float32-filterable", store: null, render: null, blend: "float32-blendable" } },
  { name: "r32uint", bytes: 4, block: 1, channels: 1, depth: false, storable: true, filterable: false, renderable: true, blendable: false, uint: true, target_cost: 4, target_align: 4, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "r8unorm", bytes: 1, block: 1, channels: 1, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 1, target_align: 1, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rg8unorm", bytes: 2, block: 1, channels: 2, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 2, target_align: 1, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rgb10a2unorm", bytes: 4, block: 1, channels: 4, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 4, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rg32float", bytes: 8, block: 1, channels: 2, depth: false, storable: true, filterable: false, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 4, needs: { filter: "float32-filterable", store: null, render: null, blend: "float32-blendable" } },
  { name: "rgba32float", bytes: 16, block: 1, channels: 4, depth: false, storable: true, filterable: false, renderable: true, blendable: true, uint: false, target_cost: 16, target_align: 4, needs: { filter: "float32-filterable", store: null, render: null, blend: "float32-blendable" } },
  { name: "rg32uint", bytes: 8, block: 1, channels: 2, depth: false, storable: true, filterable: false, renderable: true, blendable: false, uint: true, target_cost: 8, target_align: 4, needs: { filter: null, store: null, render: null, blend: null } },
  { name: "rg11b10ufloat", bytes: 4, block: 1, channels: 3, depth: false, storable: false, filterable: true, renderable: true, blendable: true, uint: false, target_cost: 8, target_align: 4, needs: { filter: null, store: null, render: "rg11b10ufloat-renderable", blend: "rg11b10ufloat-renderable" } },
  { name: "bc1-rgba-unorm", bytes: 8, block: 4, channels: 4, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-bc", store: null, render: null, blend: null } },
  { name: "bc4-r-unorm", bytes: 8, block: 4, channels: 1, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-bc", store: null, render: null, blend: null } },
  { name: "bc5-rg-unorm", bytes: 16, block: 4, channels: 2, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-bc", store: null, render: null, blend: null } },
  { name: "bc7-rgba-unorm", bytes: 16, block: 4, channels: 4, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-bc", store: null, render: null, blend: null } },
  { name: "etc2-rgb8unorm", bytes: 8, block: 4, channels: 3, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-etc2", store: null, render: null, blend: null } },
  { name: "astc-4x4-unorm", bytes: 16, block: 4, channels: 4, depth: false, storable: false, filterable: true, renderable: false, blendable: false, uint: false, target_cost: 0, target_align: 0, needs: { filter: "texture-compression-astc", store: null, render: null, blend: null } }
];
var TEXTURE_KINDS = ["2d", "3d", "2d-array", "cube"];
var MAX_COLOR_ATTACHMENTS = 8;
var MAX_LEVELS = 14;
var MAX_ANISOTROPY = 16;
var ROW_ALIGNMENT = 256;
var TEXTURE_WRITABLE = 65536;
var COMPARES = [null, "less", "less-equal", "greater", "greater-equal", "equal", "not-equal", "always", "never"];
var IMPORT_MODULE = "wrela";
var IMPORT_SUBMIT = "submit";
var IMPORT_REQUEST_STATUS = "request_status";
var IMPORT_REQUEST_TAKE = "request_take";
var IMPORT_LIMIT = "limit";
var IMPORT_INPUT = "input";
var IMPORT_PAYLOAD = "payload";
var PAYLOAD_PER_CALL = 1048576;
var IMPORT_CLOCK = "clock";
var IMPORT_PRINT = "print";
var IMPORT_PHASE = "phase";
var EVENT_SIZE = 24;
var EventKind = {
  PointerMove: 1,
  PointerDown: 2,
  PointerUp: 3,
  Wheel: 4,
  KeyDown: 5,
  KeyUp: 6,
  Text: 7,
  PadButton: 8,
  PadAxis: 9,
  Compose: 10,
  Paste: 11,
  Assist: 12,
  Message: 13,
  Connection: 14,
  Field: 15
};
var THREAD_MAIN = 0;
var THREAD_BLOCKS = 1114112;
var THREAD_BLOCK_SIZE = 8192;
var REQUEST_PENDING = -1;
var REQUEST_FAILED = -2;
var EXPORT_MEMORY = "memory";
var PANIC = 4096;
var PANIC_CAP = 4092;
var HTTP_REQUEST_MAGIC = 827410519;
var HTTP_RESPONSE_MAGIC = 827476055;
var SERVICE_RESPOND = "http/respond";
var SERVICE_TIME = "time/unix";
var MAX_HTTP_REQUEST = 1048576;
var RESPOND_WITHIN_MS = 30000;
var HTTP_METHODS = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
var SAFE_METHODS = ["GET", "HEAD", "OPTIONS"];
var OWN_HEADER_PREFIX = "x-wrela-";
var SERVER_SEES = ["accept", "accept-language", "content-type", "cookie", "if-none-match", "origin", "sec-fetch-dest", "sec-fetch-mode", "sec-fetch-site", "sec-fetch-user"];
var SERVER_SETS = ["cache-control", "content-disposition", "content-type", "etag", "last-modified", "location", "retry-after", "set-cookie", "vary", "www-authenticate"];

// ../browser/src/http.ts
var UTF8 = new TextDecoder("utf-8", { fatal: true });
var ENCODER = new TextEncoder;

class In {
  bytes;
  at = 0;
  view;
  constructor(bytes) {
    this.bytes = bytes;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }
  left() {
    return this.bytes.length - this.at;
  }
  take(n) {
    if (this.left() < n)
      throw new Error("the bytes end early");
    this.at += n;
    return this.bytes.subarray(this.at - n, this.at);
  }
  word() {
    this.take(4);
    return this.view.getUint32(this.at - 4, true);
  }
  u64() {
    this.take(8);
    return this.view.getBigUint64(this.at - 8, true);
  }
  chunk() {
    return this.take(this.word());
  }
  text() {
    return UTF8.decode(this.chunk());
  }
  headers() {
    const n = this.word();
    if (n > this.left() / 8)
      throw new Error("more headers than bytes");
    const out = [];
    for (let i = 0;i < n; i++)
      out.push([this.text(), this.text()]);
    return out;
  }
  end(what) {
    if (this.left() !== 0)
      throw new Error(`bytes past the ${what}'s end`);
  }
}

class Out {
  parts = [];
  length = 0;
  push(b) {
    this.parts.push(b);
    this.length += b.length;
    return this;
  }
  word(x) {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, x, true);
    return this.push(b);
  }
  u64(x) {
    const b = new Uint8Array(8);
    new DataView(b.buffer).setBigUint64(0, x, true);
    return this.push(b);
  }
  chunk(b) {
    return this.word(b.length).push(b);
  }
  text(s) {
    return this.chunk(ENCODER.encode(s));
  }
  headers(headers) {
    this.word(headers.length);
    for (const [n, v] of headers)
      this.text(n).text(v);
    return this;
  }
  bytes() {
    const out = new Uint8Array(this.length);
    let at = 0;
    for (const p of this.parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  }
}
function encodeRequest(q) {
  return new Out().word(HTTP_REQUEST_MAGIC).u64(q.time).text(q.method).text(q.path).text(q.query).headers(q.headers).chunk(q.body).bytes();
}
function encodeResponse(s) {
  return new Out().word(HTTP_RESPONSE_MAGIC).word(s.status).headers(s.headers).chunk(s.body).bytes();
}
function decodeResponse(bytes) {
  const r = new In(bytes);
  if (r.word() !== HTTP_RESPONSE_MAGIC)
    throw new Error("not a response");
  const status = r.word();
  if (status < 100 || status > 599)
    throw new Error(`status ${status} isn't 100 to 599`);
  const s = { status, headers: r.headers(), body: r.chunk() };
  r.end("response");
  return s;
}

// ../browser/src/wasm.ts
var UTF82 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

// ../browser/src/stream.ts
var NAMES = new Map(Object.entries(Opcode).map(([name, value]) => [value, name]));

class StreamError extends Error {
  kind;
  constructor(kind, message) {
    super(message);
    this.kind = kind;
    this.name = "StreamError";
  }
}
var tooShort = (needed, got) => new StreamError("TooShort", `batch too short: needs ${needed} bytes, has ${got}`);
var EXPECTED = {
  0: "expected no payload",
  1: "expected 1 word",
  2: "expected 2 words",
  4: "expected 4 words",
  5: "expected 5 words",
  6: "expected 6 words",
  7: "expected 7 words",
  10: "expected 10 words",
  12: "expected 12 words"
};
var padded = (n) => Math.ceil(n / 4) * 4;
function decode(batch) {
  const view = new DataView(batch.buffer, batch.byteOffset, batch.byteLength);
  const word = (at) => view.getUint32(at, true);
  if (batch.length < HEADER_LEN) {
    throw tooShort(HEADER_LEN, batch.length);
  }
  if (word(0) !== STREAM_MAGIC) {
    const magic = `[${Array.from(batch.subarray(0, 4)).join(", ")}]`;
    throw new StreamError("BadMagic", `not a command batch: magic is ${magic}, not \`WRCS\``);
  }
  const version = word(4);
  if (version !== STREAM_VERSION) {
    throw new StreamError("WrongVersion", `command stream version ${version}, but this host reads version ${STREAM_VERSION}`);
  }
  const bodyLen = word(8);
  if (bodyLen !== batch.length - HEADER_LEN) {
    throw new StreamError("BodyLength", `batch declares ${bodyLen} bytes of commands but has ${batch.length - HEADER_LEN}`);
  }
  const out = [];
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
    const bad = (why) => new StreamError("BadPayload", `malformed ${name} at byte ${at}: ${why}`);
    if (len % 4 !== 0) {
      throw bad("payload length isn't a multiple of 4");
    }
    if (batch.length - start < len) {
      throw bad("payload runs past the end of the batch");
    }
    const words = len / 4;
    const w = (i) => word(start + i * 4);
    const f = (i) => view.getFloat32(start + i * 4, true);
    const exactly = (n) => {
      if (words !== n)
        throw bad(EXPECTED[n]);
    };
    const text = (from, n, what) => {
      try {
        return UTF82.decode(batch.slice(start + from, start + from + n));
      } catch {
        throw bad(`the ${what} isn't UTF-8`);
      }
    };
    const bound = (from) => {
      if (words < from + 2)
        throw bad("payload too short");
      const n = w(from);
      if (words < from + 1 + 3 * n + 1)
        throw bad("binding list runs past the payload");
      const bindings = [];
      for (let k = 0;k < n; k++) {
        const i = from + 1 + 3 * k;
        bindings.push({ handle: w(i), offset: w(i + 1), size: w(i + 2) });
      }
      const ulen = w(from + 1 + 3 * n);
      const ustart = (from + 2 + 3 * n) * 4;
      if (ulen !== len - ustart)
        throw bad("uniform length doesn't match the payload");
      return [bindings, batch.subarray(start + ustart, start + len)];
    };
    const twoRuns = (what, mismatch) => {
      if (words < 3)
        throw bad("expected at least 3 words");
      const n = w(1);
      const m = w(2);
      if (padded(n) + padded(m) !== len - 12)
        throw bad(mismatch);
      const from = start + 12 + padded(n);
      return [text(12, n, what), batch.subarray(from, from + m)];
    };
    let cmd;
    switch (name) {
      case "CreateBuffer": {
        exactly(2);
        const size = w(1);
        if (size === 0 || size % 4 !== 0)
          throw bad("size must be a positive multiple of 4");
        cmd = { op: name, handle: w(0), size };
        break;
      }
      case "WriteBuffer": {
        if (words < 3)
          throw bad("expected at least 3 words");
        const offset = w(1);
        const n = w(2);
        if (offset % 4 !== 0 || n % 4 !== 0)
          throw bad("offset and length must be multiples of 4");
        if (n !== len - 12)
          throw bad("data length doesn't match the payload");
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
        if (w(2) % 4 !== 0)
          throw bad("the arguments' offset must be a multiple of 4");
        cmd = { op: name, pipeline: w(0), arguments: w(1), offset: w(2), bindings, uniforms };
        break;
      }
      case "DrawIndexedIndirect": {
        const [bindings, uniforms] = bound(6);
        if (w(2) % 4 !== 0 || w(3) % 4 !== 0)
          throw bad("the indices' offset and size must be multiples of 4");
        if (w(5) % 4 !== 0)
          throw bad("the arguments' offset must be a multiple of 4");
        cmd = {
          op: name,
          pipeline: w(0),
          indices: w(1),
          index_offset: w(2),
          index_size: w(3),
          arguments: w(4),
          offset: w(5),
          bindings,
          uniforms
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
        if ([w(1), w(3), w(4)].some((x) => x % 4 !== 0))
          throw bad("offsets and size must be multiples of 4");
        cmd = { op: name, source: w(0), sourceOffset: w(1), destination: w(2), destinationOffset: w(3), size: w(4) };
        break;
      }
      case "CreateTexture": {
        exactly(7);
        const writable = (w(3) & TEXTURE_WRITABLE) !== 0;
        const format = TEXTURE_FORMATS[w(3) & ~TEXTURE_WRITABLE];
        if (format === undefined)
          throw bad("unknown texture format");
        const kind = TEXTURE_KINDS[w(4)];
        if (kind === undefined)
          throw bad("unknown texture kind");
        const layers = w(5);
        const levels = w(6);
        if (w(1) === 0 || w(2) === 0 || layers === 0)
          throw bad("a texture's width, height and depth or layers are positive");
        if (levels === 0 || levels > MAX_LEVELS)
          throw bad("a texture has 1 to 14 mip levels");
        if (writable && !format.storable)
          throw bad("kernels write only a format the format table says they can");
        if (kind === "2d" && layers !== 1)
          throw bad("a 2D texture has one layer");
        if (kind === "cube" && (layers !== 6 || w(1) !== w(2)))
          throw bad("a cube has six square layers");
        if (kind === "3d" && (format.depth || format.block > 1))
          throw bad("a 3D texture holds uncompressed colours");
        if (kind === "3d" && levels !== 1)
          throw bad("a 3D texture has one level");
        cmd = { op: name, handle: w(0), width: w(1), height: w(2), format: format.name, writable, kind, layers, levels };
        break;
      }
      case "WriteTexture": {
        if (words < 8)
          throw bad("expected at least 8 words");
        if (padded(w(7)) !== len - 32)
          throw bad("data length doesn't match the payload");
        const data = batch.subarray(start + 32, start + 32 + w(7));
        cmd = { op: name, handle: w(0), level: w(1), layer: w(2), x: w(3), y: w(4), width: w(5), height: w(6), data };
        break;
      }
      case "CreateSampler": {
        exactly(6);
        if (w(1) > 1 || w(2) > 1 || w(4) > 1)
          throw bad("a sampler's filters and address are 0 or 1");
        if (w(5) === 0 || w(5) > MAX_ANISOTROPY)
          throw bad("a sampler's anisotropy is 1 to 16");
        if (w(5) > 1 && (w(1) === 0 || w(4) === 0))
          throw bad("an anisotropic sampler filters linearly, between levels too");
        let compare = null;
        if (w(3) !== 0) {
          compare = COMPARES[w(3)] ?? null;
          if (compare === null)
            throw bad("unknown comparison");
        }
        cmd = { op: name, handle: w(0), linear: w(1) === 1, repeat: w(2) === 1, compare, mipLinear: w(4) === 1, anisotropy: w(5) };
        break;
      }
      case "BeginPass": {
        if (words < 2)
          throw bad("expected at least 2 words");
        const n = w(0);
        if (n > MAX_COLOR_ATTACHMENTS)
          throw bad("a pass has at most eight colour targets");
        if (words !== 2 + 8 * n + 4)
          throw bad("the colour targets don't fill the payload");
        if (w(1) > 1)
          throw bad("the join word is 0 or 1");
        const colors = [];
        for (let k = 0;k < n; k++) {
          const c = 2 + 8 * k;
          if (w(c + 3) > 1)
            throw bad("a load is 0 (clear) or 1 (keep)");
          colors.push({ texture: w(c), level: w(c + 1), layer: w(c + 2), keep: w(c + 3) === 1, clear: [f(c + 4), f(c + 5), f(c + 6), f(c + 7)] });
        }
        if (colors.some((c) => c.texture === SCREEN) && n !== 1)
          throw bad("a pass on the screen has the screen as its one colour");
        const d = 2 + 8 * n;
        if (w(d + 2) > 1)
          throw bad("a load is 0 (clear) or 1 (keep)");
        const pass = { colors, join: w(1) === 1, depth: w(d), depthLayer: w(d + 1), keepDepth: w(d + 2) === 1, clearDepth: f(d + 3) };
        cmd = { op: name, pass };
        break;
      }
      case "CopyTexture": {
        exactly(12);
        cmd = {
          op: name,
          source: { texture: w(0), level: w(1), layer: w(2), x: w(3), y: w(4), width: w(10), height: w(11) },
          destination: { texture: w(5), level: w(6), layer: w(7), x: w(8), y: w(9) }
        };
        break;
      }
      case "CopyTextureToBuffer": {
        exactly(10);
        if (w(8) % ROW_ALIGNMENT !== 0 || w(9) % ROW_ALIGNMENT !== 0 || w(9) === 0)
          throw bad("the buffer's offset and bytes per row are multiples of 256");
        const source = { texture: w(0), level: w(1), layer: w(2), x: w(3), y: w(4), width: w(5), height: w(6) };
        cmd = { op: name, source, buffer: w(7), offset: w(8), bytesPerRow: w(9) };
        break;
      }
      case "CopyBufferToTexture": {
        exactly(10);
        if (w(1) % 4 !== 0 || w(2) % ROW_ALIGNMENT !== 0 || w(2) === 0)
          throw bad("the buffer's offset is a multiple of 4, its bytes per row of 256");
        const destination = { texture: w(3), level: w(4), layer: w(5), x: w(6), y: w(7), width: w(8), height: w(9) };
        cmd = { op: name, buffer: w(0), offset: w(1), bytesPerRow: w(2), destination };
        break;
      }
      case "SetScissor": {
        exactly(4);
        cmd = { op: name, x: w(0), y: w(1), width: w(2), height: w(3) };
        break;
      }
      case "ConfigureScreen": {
        exactly(2);
        if (w(0) > 1 || w(1) > 1)
          throw bad("a screen's `hdr` is 0 or 1, and its gamut 0 (sRGB) or 1 (Display P3)");
        cmd = { op: name, hdr: w(0) === 1, gamut: w(1) };
        break;
      }
      case "SetViewport": {
        exactly(6);
        const rect = [f(0), f(1), f(2), f(3)];
        const minDepth = f(4);
        const maxDepth = f(5);
        if (!rect.every(Number.isFinite) || rect[2] < 0 || rect[3] < 0)
          throw bad("a viewport is a finite rectangle of non-negative size");
        if (!(minDepth >= 0 && minDepth <= 1) || !(maxDepth >= 0 && maxDepth <= 1) || minDepth > maxDepth)
          throw bad("a viewport's depths are in [0, 1], the least first");
        cmd = { op: name, rect, minDepth, maxDepth };
        break;
      }
      case "DrawIndexed": {
        const [bindings, uniforms] = bound(9);
        if (w(2) % 4 !== 0 || w(3) % 4 !== 0)
          throw bad("the indices' offset and size must be multiples of 4");
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
          uniforms
        };
        break;
      }
      case "MultiDrawIndirect":
      case "MultiDrawIndexedIndirect": {
        const indexed = name === "MultiDrawIndexedIndirect";
        const at = indexed ? 4 : 1;
        const [bindings, uniforms] = bound(at + 5);
        if (indexed && (w(2) % 4 !== 0 || w(3) % 4 !== 0))
          throw bad("the indices' offset and size must be multiples of 4");
        if (w(at + 1) % 4 !== 0 || w(at + 4) % 4 !== 0)
          throw bad("the arguments' and the count's offsets must be multiples of 4");
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
          uniforms
        };
        break;
      }
      case "ReadBuffer": {
        exactly(4);
        if (w(2) % 4 !== 0 || w(3) % 4 !== 0)
          throw bad("offset and size must be multiples of 4");
        cmd = { op: name, request: w(0), handle: w(1), offset: w(2), size: w(3) };
        break;
      }
      case "StorageRead":
      case "Fetch":
      case "Prepare":
      case "HandOver":
      case "Timed": {
        if (words < 2)
          throw bad("expected at least 2 words");
        const n = w(1);
        if (padded(n) !== len - 8)
          throw bad("the text's length doesn't match the payload");
        const t = text(8, n, "text");
        if (name === "Fetch")
          cmd = { op: name, request: w(0), url: t };
        else if (name === "Timed")
          cmd = { op: name, request: w(0), label: t };
        else
          cmd = { op: name, request: w(0), path: t };
        break;
      }
      case "Label":
      case "Unprepare": {
        if (words < 1)
          throw bad("expected at least 1 word");
        const n = w(0);
        if (padded(n) !== len - 4)
          throw bad("the text's length doesn't match the payload");
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

// src/host.ts
var EXPORT_SERVE = "serve";
var EXPORT_SERVE_INIT = "serve_init";
var MAX_KEY = 1024;
var MAX_VALUE = 1020 << 10;
var MAX_SCAN = 1 << 20;
var MAX_RANDOM = 1024;
var LIMITS = { memory: 64 << 20, commands: 16 << 20, turn: 250 };

class Failure extends Error {
  program;
  constructor(program, why) {
    super(why);
    this.program = program;
  }
}
var UTF83 = new TextDecoder("utf-8", { fatal: true });
var ENCODER2 = new TextEncoder;

class Bytes {
  bytes;
  at = 0;
  view;
  constructor(bytes) {
    this.bytes = bytes;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }
  left() {
    return this.bytes.length - this.at;
  }
  take(n) {
    if (this.left() < n)
      throw new Error("a service's bytes end early");
    const out = this.bytes.subarray(this.at, this.at + n);
    this.at += n;
    return out;
  }
  byte() {
    return this.take(1)[0];
  }
  word() {
    const w = this.view.getUint32(this.take(4).byteOffset - this.bytes.byteOffset, true);
    return w;
  }
  u64() {
    const at = this.take(8).byteOffset - this.bytes.byteOffset;
    return this.view.getBigUint64(at, true);
  }
  chunk() {
    return this.take(this.word());
  }
  text() {
    return UTF83.decode(this.chunk());
  }
  end() {
    if (this.left() !== 0)
      throw new Error("a service's bytes go on past their end");
  }
}

class Out2 {
  parts = [];
  length = 0;
  push(b) {
    this.parts.push(b);
    this.length += b.length;
    return this;
  }
  word(x) {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, x, true);
    return this.push(b);
  }
  u64(x) {
    const b = new Uint8Array(8);
    new DataView(b.buffer).setBigUint64(0, x, true);
    return this.push(b);
  }
  chunk(b) {
    return this.word(b.length).push(b);
  }
  text(s) {
    return this.chunk(ENCODER2.encode(s));
  }
  bytes() {
    const out = new Uint8Array(this.length);
    let at = 0;
    for (const p of this.parts) {
      out.set(p, at);
      at += p.length;
    }
    return out;
  }
}
var listed = (list, name) => name.startsWith(OWN_HEADER_PREFIX) || list.includes(name);
var setByServer = (name) => listed(SERVER_SETS, name);
var HEADER_NAME = /^[a-z0-9!#$%&'*+.^_`|~-]+$/;
var HEADER_VALUE = /^[^\r\n\0]*$/;
function checkResponse(bytes) {
  const s = decodeResponse(bytes);
  for (const [n, v] of s.headers) {
    if (!HEADER_NAME.test(n))
      throw new Error(`\`${n}\` isn't a header's name in lower case`);
    if (!setByServer(n))
      throw new Error(`a server can't set header \`${n}\``);
    if (!HEADER_VALUE.test(v))
      throw new Error(`header \`${n}\`'s value has a line break`);
  }
  return s;
}
function withHostHeaders(s) {
  const headers = [...s.headers];
  if (!headers.some(([n]) => n === "cache-control"))
    headers.push(["cache-control", "no-store"]);
  headers.push(["x-content-type-options", "nosniff"], ["referrer-policy", "same-origin"], ["content-security-policy", "default-src 'none'"]);
  return { ...s, headers };
}
var bare = (status) => encodeResponse(withHostHeaders({ status, headers: [], body: new Uint8Array(0) }));
var asBytes = (v) => v instanceof Uint8Array ? v : new Uint8Array(v);

class Store {
  storage;
  constructor(storage) {
    this.storage = storage;
    storage.sql.exec("CREATE TABLE IF NOT EXISTS kv (key BLOB PRIMARY KEY, value BLOB NOT NULL) WITHOUT ROWID");
  }
  get(body) {
    if (body.length > MAX_KEY)
      throw new Error(`a key is longer than ${MAX_KEY} bytes`);
    const rows = this.storage.sql.exec("SELECT value FROM kv WHERE key = ?", body).toArray();
    if (rows.length === 0)
      return new Uint8Array([0]);
    return new Out2().push(new Uint8Array([1])).push(asBytes(rows[0]["value"])).bytes();
  }
  scan(body) {
    const r = new Bytes(body);
    const from = r.chunk();
    const to = r.chunk();
    const limit = r.word();
    const down = r.byte() === 1;
    r.end();
    if (from.length > MAX_KEY || to.length > MAX_KEY)
      throw new Error(`a key is longer than ${MAX_KEY} bytes`);
    const order = down ? "DESC" : "ASC";
    const rows = to.length === 0 ? this.storage.sql.exec(`SELECT key, value FROM kv WHERE key >= ? ORDER BY key ${order} LIMIT ?`, from, limit) : this.storage.sql.exec(`SELECT key, value FROM kv WHERE key >= ? AND key < ? ORDER BY key ${order} LIMIT ?`, from, to, limit);
    const out = new Out2;
    let count = 0;
    let bytes = 0;
    for (const row of rows.toArray()) {
      const k = asBytes(row["key"]);
      const v = asBytes(row["value"]);
      bytes += k.length + v.length;
      if (count > 0 && bytes > MAX_SCAN)
        break;
      out.chunk(k).chunk(v);
      count++;
    }
    return new Out2().word(count).push(out.bytes()).bytes();
  }
  commit(batches) {
    const ops = [];
    for (const b of batches) {
      const r = new Bytes(b);
      while (r.left() > 0) {
        const kind = r.byte();
        const k = r.chunk();
        if (k.length > MAX_KEY)
          throw new Failure(true, `a key is longer than ${MAX_KEY} bytes`);
        if (kind === 1) {
          const v = r.chunk();
          if (v.length > MAX_VALUE)
            throw new Failure(true, `a value is longer than ${MAX_VALUE} bytes`);
          ops.push([k, v]);
        } else if (kind === 2) {
          ops.push([k, null]);
        } else {
          throw new Failure(true, `a store write of kind ${kind}`);
        }
      }
    }
    if (ops.length === 0)
      return;
    this.storage.transactionSync(() => {
      for (const [k, v] of ops) {
        if (v === null)
          this.storage.sql.exec("DELETE FROM kv WHERE key = ?", k);
        else
          this.storage.sql.exec("INSERT OR REPLACE INTO kv (key, value) VALUES (?, ?)", k, v);
      }
    });
  }
}
function random(body) {
  const r = new Bytes(body);
  const n = r.word();
  r.end();
  if (n > MAX_RANDOM)
    throw new Error(`more than ${MAX_RANDOM} random bytes at once`);
  return crypto.getRandomValues(new Uint8Array(n));
}
function derToRaw(der) {
  if (der.length < 8 || der[0] !== 48 || der[1] !== der.length - 2)
    return null;
  const out = new Uint8Array(64);
  let at = 2;
  for (const half of [0, 32]) {
    if (der[at] !== 2)
      return null;
    const n = der[at + 1];
    at += 2;
    if (n === 0 || n > 33 || at + n > der.length)
      return null;
    let int = der.subarray(at, at + n);
    at += n;
    if (int[0] & 128)
      return null;
    if (int[0] === 0) {
      if (n === 1 || !(int[1] & 128))
        return null;
      int = int.subarray(1);
    }
    if (int.length > 32)
      return null;
    out.set(int, half + 32 - int.length);
  }
  return at === der.length ? out : null;
}
var base64url = (b) => {
  let s = "";
  for (const x of b)
    s += String.fromCharCode(x);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
};
var unsigned = (b) => {
  let i = 0;
  while (i < b.length - 1 && b[i] === 0)
    i++;
  return b.subarray(i);
};
async function verify(body) {
  const r = new Bytes(body);
  const alg = r.byte();
  const key = r.chunk().slice();
  const message = r.chunk().slice();
  const signature = r.chunk().slice();
  r.end();
  const answer = (ok) => new Uint8Array([ok ? 1 : 0]);
  const imported = async (format, data, algorithm) => {
    try {
      return await (format === "jwk" ? crypto.subtle.importKey("jwk", data, algorithm, false, ["verify"]) : crypto.subtle.importKey("raw", data, algorithm, false, ["verify"]));
    } catch {
      throw new Error("the key isn't one of the algorithm's");
    }
  };
  switch (alg) {
    case 1: {
      const k = await imported("raw", key, { name: "ECDSA", namedCurve: "P-256" });
      const raw = derToRaw(signature);
      if (raw === null)
        return answer(false);
      return answer(await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, k, raw, message));
    }
    case 2: {
      if (key.length !== 32)
        throw new Error("an Ed25519 key is 32 bytes");
      const k = await imported("raw", key, { name: "Ed25519" });
      if (signature.length !== 64)
        return answer(false);
      return answer(await crypto.subtle.verify({ name: "Ed25519" }, k, signature, message));
    }
    case 3: {
      const kr = new Bytes(key);
      const n = unsigned(kr.chunk());
      const e = unsigned(kr.chunk());
      kr.end();
      const jwk = { kty: "RSA", n: base64url(n), e: base64url(e), alg: "RS256", ext: true };
      const k = await imported("jwk", jwk, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" });
      return answer(await crypto.subtle.verify({ name: "RSASSA-PKCS1-v1_5" }, k, signature, message));
    }
    default:
      throw new Error(`no signature algorithm ${alg}`);
  }
}
function event(kind, a, b, c, d) {
  const out = new Uint8Array(EVENT_SIZE);
  const v = new DataView(out.buffer);
  for (const [i, w] of [kind, 0, a, b, c, d].entries())
    v.setUint32(4 * i, w >>> 0, true);
  return out;
}

class Instance {
  host;
  instance;
  memory;
  serve;
  answered = new Map;
  arriving = [];
  commands = [];
  commandBytes = 0;
  refused = null;
  responding = new Set;
  constructor(host, instance) {
    this.host = host;
    this.instance = instance;
    const exports = instance.exports;
    const memory = exports[EXPORT_MEMORY];
    if (!(memory instanceof WebAssembly.Memory))
      throw new Failure(false, "this isn't a server build: its memory isn't its own");
    this.memory = memory;
    const serve = exports[EXPORT_SERVE];
    if (typeof serve !== "function")
      throw new Failure(false, "this isn't a server build: it doesn't export `serve`");
    this.serve = serve;
  }
  static async start(host, module) {
    let self = null;
    const me = () => {
      if (self === null)
        throw new Error("the instance isn't started");
      return self;
    };
    const imports = { [IMPORT_MODULE]: host.imports(me) };
    let instance;
    try {
      instance = await WebAssembly.instantiate(module, imports);
    } catch (e) {
      throw new Failure(false, `instantiating the server build: ${e.message}`);
    }
    self = new Instance(host, instance);
    const init = instance.exports[EXPORT_SERVE_INIT];
    if (init !== undefined) {
      if (typeof init !== "function" || init.length !== 0)
        throw new Failure(false, "`serve_init` isn't `serve_init()`");
      self.call(() => init());
    }
    return self;
  }
  call(f) {
    this.refused = null;
    this.responding.clear();
    try {
      f();
    } catch (e) {
      throw this.classify(e);
    } finally {
      this.host.queue.endCall();
    }
    if (this.memory.buffer.byteLength > this.host.limits.memory) {
      throw new Failure(false, "the program asked for more memory than its instance has");
    }
  }
  classify(e) {
    if (e instanceof Failure)
      return e;
    if (this.refused !== null)
      return new Failure(true, this.refused);
    if (e instanceof RangeError && /call stack/i.test(e.message))
      return new Failure(false, "the call went deeper than the stack allows");
    if (e instanceof WebAssembly.RuntimeError) {
      const panic = this.panicMessage();
      return new Failure(true, panic ?? e.message);
    }
    return new Failure(false, e?.message ?? String(e));
  }
  panicMessage() {
    const at = THREAD_BLOCKS + THREAD_MAIN * THREAD_BLOCK_SIZE + PANIC;
    if (at + 4 + PANIC_CAP > this.memory.buffer.byteLength)
      return null;
    const view = new DataView(this.memory.buffer);
    const n = Math.min(view.getUint32(at, true), PANIC_CAP);
    if (n === 0)
      return null;
    view.setUint32(at, 0, true);
    return new TextDecoder().decode(new Uint8Array(this.memory.buffer, at + 4, n));
  }
  refuse(why, limit = false) {
    this.refused = why;
    throw new Failure(!limit, why);
  }
  read(at, len) {
    if (at + len > this.memory.buffer.byteLength)
      this.refuse(`(${at}, ${len}) reaches past the end of the program's memory`);
    return new Uint8Array(this.memory.buffer, at, len).slice();
  }
  write(at, bytes, what) {
    if (at + bytes.length > this.memory.buffer.byteLength)
      this.refuse(`${what} reaches past the end of the program's memory`);
    new Uint8Array(this.memory.buffer, at, bytes.length).set(bytes);
  }
}

class Queue {
  events = [];
  payloads = new Map;
  delivered = [];
  serial = 0;
  fresh = false;
  push(e) {
    this.events.push({ event: e, serial: 0 });
    this.fresh = true;
  }
  pushMessage(conn, bytes) {
    const serial = ++this.serial;
    this.events.push({ event: event(EventKind.Message, conn, serial >>> 0, Math.floor(serial / 2 ** 32), bytes.length), serial });
    this.payloads.set(serial, bytes);
    this.fresh = true;
  }
  take(cap) {
    let given = this.delivered.reduce((n, s) => n + (this.payloads.get(s)?.length ?? 0), 0);
    const out = [];
    while (out.length < cap && this.events.length > 0) {
      const q = this.events[0];
      if (q.serial !== 0) {
        const len = this.payloads.get(q.serial)?.length ?? 0;
        if (given !== 0 && given + len > PAYLOAD_PER_CALL)
          break;
        given += len;
        this.delivered.push(q.serial);
      }
      this.events.shift();
      out.push(q.event);
    }
    const bytes = new Uint8Array(out.length * EVENT_SIZE);
    out.forEach((e, i) => bytes.set(e, i * EVENT_SIZE));
    return bytes;
  }
  payload(serial) {
    return this.delivered.includes(serial) ? this.payloads.get(serial) ?? null : null;
  }
  endCall() {
    for (const s of this.delivered)
      this.payloads.delete(s);
    this.delivered = [];
  }
  clear() {
    this.events = [];
    this.payloads.clear();
    this.delivered = [];
    this.fresh = false;
  }
}
var POISONED = 2 ** 31;

class Server {
  module;
  options;
  limits;
  store;
  queue = new Queue;
  instance = null;
  starting = null;
  open = new Map;
  nextConn = 1;
  started;
  lastClock = 0;
  pumping = false;
  again = false;
  answeredLast = false;
  now;
  constructor(module, storage, options = {}) {
    this.module = module;
    this.options = options;
    this.limits = options.limits ?? LIMITS;
    this.now = options.now ?? Date.now;
    this.store = new Store(storage);
    this.started = this.now();
  }
  request(bytes) {
    const conn = this.nextConn++;
    if (this.nextConn >= POISONED)
      this.nextConn = 1;
    const answer = new Promise((resolve) => {
      const timer = setTimeout(() => this.finish(conn, bare(504)), RESPOND_WITHIN_MS);
      this.open.set(conn, { resolve, timer });
    });
    this.queue.push(event(EventKind.Connection, conn, 1, 0, 0));
    this.queue.pushMessage(conn, bytes);
    this.pump();
    return answer;
  }
  async alarm() {
    this.answeredLast = true;
    await this.pump();
  }
  due() {
    return this.queue.fresh || this.answeredLast || (this.instance?.arriving.length ?? 0) > 0;
  }
  async pump() {
    if (this.pumping) {
      this.again = true;
      return;
    }
    this.pumping = true;
    try {
      const began = this.now();
      do {
        this.again = false;
        while (this.due()) {
          await this.once();
          if (this.open.size === 0 && this.due() && this.now() - began >= this.limits.turn) {
            this.options.setAlarm?.(this.now());
            return;
          }
          await new Promise((r) => setTimeout(r, 0));
        }
      } while (this.again);
    } finally {
      this.pumping = false;
    }
  }
  async once() {
    let instance;
    try {
      instance = await this.ready();
    } catch (e) {
      this.fail(e);
      return;
    }
    const arrived = await Promise.all(instance.arriving.splice(0));
    for (const [id, answer] of arrived)
      instance.answered.set(id, answer);
    if (instance !== this.instance)
      return;
    this.queue.fresh = false;
    instance.commands = [];
    instance.commandBytes = 0;
    try {
      const time = Math.fround((this.now() - this.started) / 1000);
      instance.call(() => instance.serve(time));
      this.answeredLast = instance.arriving.length > 0;
      this.carryOut(instance.commands);
    } catch (e) {
      this.fail(e);
    }
  }
  ready() {
    if (this.instance !== null)
      return Promise.resolve(this.instance);
    this.starting ??= Instance.start(this, this.module).then((i) => {
      this.instance = i;
      this.starting = null;
      return i;
    }, (e) => {
      this.starting = null;
      throw e;
    });
    return this.starting;
  }
  carryOut(commands) {
    this.store.commit(commands.flatMap((c) => c.kind === "write" ? [c.body] : []));
    for (const c of commands) {
      if (c.kind === "respond")
        this.finish(c.conn, encodeResponse(withHostHeaders(c.response)));
    }
  }
  finish(conn, response) {
    const o = this.open.get(conn);
    if (o === undefined)
      return;
    clearTimeout(o.timer);
    this.open.delete(conn);
    o.resolve(response);
    if (this.instance !== null) {
      this.queue.push(event(EventKind.Connection, conn, 0, 0, 0));
      this.pump();
    }
  }
  fail(e) {
    const f = e instanceof Failure ? e : new Failure(false, String(e?.message ?? e));
    this.options.log?.(`wrela: the server's call failed: ${f.program ? "the program's: " : "the host's limits: "}${f.message}`);
    this.instance = null;
    this.answeredLast = false;
    this.queue.clear();
    for (const conn of [...this.open.keys()])
      this.finish(conn, bare(f.program ? 500 : 503));
  }
  imports(me) {
    const host = this;
    return {
      [IMPORT_SUBMIT](at, len) {
        const self = me();
        let commands;
        try {
          commands = decode(self.read(at, len));
        } catch (e) {
          self.refuse(`a malformed command batch: ${e.message}`);
        }
        for (const c of commands) {
          if (c.op !== "HostRequest")
            self.refuse(`a server host can't carry out ${c.op}`);
          host.submit(self, c.request, c.service, c.body.slice());
        }
      },
      [IMPORT_REQUEST_STATUS](id) {
        const self = me();
        if (!self.answered.has(id))
          return REQUEST_PENDING;
        const a = self.answered.get(id);
        return a === null || a === undefined ? REQUEST_FAILED : a.length;
      },
      [IMPORT_REQUEST_TAKE](id, at) {
        const self = me();
        if (!self.answered.has(id))
          self.refuse(`request_take(${id}): the request isn't answered`);
        const a = self.answered.get(id) ?? new Uint8Array(0);
        self.write(at, a, `request_take(${id}, ${at})`);
        self.answered.delete(id);
      },
      [IMPORT_LIMIT]: () => 0,
      [IMPORT_PRINT](at, len) {
        const line = new TextDecoder().decode(me().read(at, len));
        host.options.print?.(line);
      },
      [IMPORT_INPUT](at, cap) {
        const self = me();
        const n = Math.min(cap, host.queue.events.length);
        if (at + n * EVENT_SIZE > self.memory.buffer.byteLength)
          self.refuse(`input(${at}, ${cap}) reaches past the end of the program's memory`);
        const bytes = host.queue.take(cap);
        self.write(at, bytes, `input(${at}, ${cap})`);
        return bytes.length / EVENT_SIZE;
      },
      [IMPORT_PAYLOAD](lo, hi, at, cap) {
        const self = me();
        const bytes = host.queue.payload((lo >>> 0) + (hi >>> 0) * 2 ** 32);
        if (bytes === null)
          return -1;
        self.write(at, bytes.subarray(0, Math.min(cap, bytes.length)), `payload(.., ${at}, ${cap})`);
        return bytes.length;
      },
      [IMPORT_CLOCK](at) {
        host.lastClock = Math.max(host.lastClock, (host.now() - host.started) / 1000);
        const b = new Uint8Array(8);
        new DataView(b.buffer).setFloat64(0, host.lastClock, true);
        me().write(at, b, `clock(${at})`);
      },
      [IMPORT_PHASE]: () => {}
    };
  }
  submit(self, id, service, body) {
    const command = (c, bytes) => {
      self.commandBytes += bytes;
      if (self.commandBytes > this.limits.commands)
        self.refuse(`a call's commands past ${this.limits.commands} bytes`, true);
      self.commands.push(c);
    };
    const answer = (a) => {
      self.arriving.push(Promise.resolve(a).then((x) => [id, x]));
    };
    switch (service) {
      case "store/write":
        return command({ kind: "write", body }, body.length);
      case SERVICE_RESPOND: {
        if (body.length < 4)
          self.refuse("`http/respond` needs a connection's number");
        const conn = new DataView(body.buffer, body.byteOffset).getUint32(0, true);
        if (!this.open.has(conn))
          return;
        if (self.responding.has(conn))
          self.refuse(`request ${conn} takes one response`);
        let response;
        try {
          response = checkResponse(body.subarray(4));
        } catch (e) {
          self.refuse(`\`http/respond\`: ${e.message}`);
        }
        self.responding.add(conn);
        return command({ kind: "respond", conn, response }, body.length);
      }
      case "net/send":
      case "net/close":
      case "net/tag":
      case "net/keep":
      case "net/compact":
        return self.refuse(`\`${service}\`: an HTTP server's connections take one response (\`std::http::respond\`)`);
      case "store/get":
      case "store/scan":
      case "crypto/random":
      case SERVICE_TIME:
        try {
          return answer(this.answerNow(service, body));
        } catch {
          return answer(null);
        }
      case "crypto/verify":
        return answer(verify(body).catch(() => null));
      default:
        return answer(null);
    }
  }
  answerNow(service, body) {
    switch (service) {
      case "store/get":
        return this.store.get(body);
      case "store/scan":
        return this.store.scan(body);
      case "crypto/random":
        return random(body);
      default: {
        if (body.length !== 0)
          throw new Error("`time/unix` takes no bytes");
        return new Out2().u64(BigInt(Math.floor(this.now()))).bytes();
      }
    }
  }
}

// src/cloudflare.ts
var SERVER_OBJECT = "server";
var seen = (name) => name.startsWith(OWN_HEADER_PREFIX) || SERVER_SEES.includes(name);
function refusal(status, headers = {}) {
  return new Response(null, { status, headers: { "cache-control": "no-store", ...headers } });
}
async function bodyWithin(request, max) {
  const declared = request.headers.get("content-length");
  if (declared !== null && Number(declared) > max)
    return null;
  if (request.body === null)
    return new Uint8Array(0);
  const parts = [];
  let length = 0;
  const reader = request.body.getReader();
  for (;; ) {
    const { done, value } = await reader.read();
    if (done)
      break;
    length += value.length;
    if (length > max) {
      await reader.cancel();
      return null;
    }
    parts.push(value);
  }
  const out = new Uint8Array(length);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}
function shareable(r) {
  const control = r.headers.get("cache-control") ?? "";
  const life = /(?:^|[\s,])(?:s-maxage|max-age)=(\d+)/.exec(control);
  return r.status === 200 && /(?:^|[\s,])public(?:$|[\s,])/.test(control) && life !== null && Number(life[1]) > 0 && !/(?:^|[\s,])(?:private|no-store|no-cache)/.test(control) && !r.headers.has("set-cookie") && !r.headers.has("vary");
}
function limits(env) {
  const list = JSON.parse(env.WRELA_LIMITS ?? "[]");
  if (!Array.isArray(list) || !list.every((l) => typeof l?.binding === "string" && typeof l?.prefix === "string")) {
    throw new Error("WRELA_LIMITS isn't a list of { binding, prefix, methods?, period? }");
  }
  return list;
}
async function pastLimit(env, method, path, address) {
  for (const l of limits(env)) {
    if (!path.startsWith(l.prefix) || l.methods !== undefined && !l.methods.includes(method))
      continue;
    const binding = env[l.binding];
    if (typeof binding?.limit !== "function")
      throw new Error(`WRELA_LIMITS names \`${l.binding}\`, which isn't a rate limit binding`);
    if (!(await binding.limit({ key: address })).success)
      return l;
  }
  return null;
}
async function api(request, env, options = {}) {
  const url = new URL(request.url);
  const prefix = options.prefix ?? "/api/";
  if (!url.pathname.startsWith(prefix))
    return null;
  const method = request.method.toUpperCase();
  if (!HTTP_METHODS.includes(method))
    return refusal(405, { allow: HTTP_METHODS.join(", ") });
  const limited = await pastLimit(env, method, url.pathname, request.headers.get("cf-connecting-ip") ?? "unknown");
  if (limited !== null)
    return refusal(429, { "retry-after": String(limited.period ?? 60) });
  const cache = options.cache ?? globalThis.caches?.default;
  const key = new Request(url.href, { method: "GET" });
  if (method === "GET" && cache !== undefined) {
    const hit = await cache.match(key);
    if (hit !== undefined)
      return hit;
  }
  if (!SAFE_METHODS.includes(method)) {
    const origins = (env.WRELA_ORIGINS ?? "").split(",").map((o) => o.trim()).filter((o) => o !== "");
    const origin = request.headers.get("origin");
    const site = request.headers.get("sec-fetch-site");
    if (origin === null || !origins.includes(origin) || site !== null && site !== "same-origin")
      return refusal(403);
  }
  const body = await bodyWithin(request, MAX_HTTP_REQUEST);
  if (body === null)
    return refusal(413);
  const headers = [];
  for (const [name, value] of request.headers)
    if (seen(name))
      headers.push([name, value]);
  const bytes = encodeRequest({ time: BigInt(Date.now()), method, path: url.pathname, query: url.search.slice(1), headers, body });
  if (bytes.length > MAX_HTTP_REQUEST)
    return refusal(413);
  let answer;
  try {
    answer = await env.WRELA_SERVER.getByName(SERVER_OBJECT).request(bytes);
  } catch {
    answer = bare(503);
  }
  const s = decodeResponse(answer);
  const out = new Headers;
  for (const [n, v] of s.headers)
    out.append(n, v);
  const empty = method === "HEAD" || s.status === 204 || s.status === 304;
  const response = new Response(empty ? null : s.body.slice(), { status: s.status, headers: out });
  if (method === "GET" && cache !== undefined && shareable(response)) {
    const kept = cache.put(key, response.clone());
    if (options.ctx !== undefined)
      options.ctx.waitUntil(kept);
    else
      await kept;
  }
  return response;
}
function serverClass(module) {
  return class WrelaServer extends DurableObject {
    server;
    constructor(ctx, env) {
      super(ctx, env);
      this.server = new Server(module, ctx.storage, {
        setAlarm: (at) => void ctx.storage.setAlarm(at),
        log: (line) => console.error(line),
        ...env.WRELA_PRINT === "1" ? { print: (line) => console.log(line) } : {}
      });
    }
    async request(bytes) {
      return this.server.request(new Uint8Array(bytes));
    }
    async alarm() {
      await this.server.alarm();
    }
  };
}

// src/entry.ts
var WrelaServer = serverClass(module);
export {
  WrelaServer,
  api
};
