// Checks each command against the manifest and the program's resources before it runs. Mirrors
// runtime/abi/src/check.rs (same rules, same messages, checked by the ABI's test vectors), so
// both hosts reject the same programs at the command that's wrong, rather than with an
// asynchronous WebGPU error that names no command.

import {
  BINDING_OFFSET_ALIGNMENT,
  DEFAULT_LIMITS,
  FEATURES,
  MAX_COLOR_ATTACHMENT_BYTES,
  MAX_LAYERS,
  MAX_TEXTURE_3D,
  NONE,
  SCREEN,
  SCREEN_FORMAT,
} from "./abi.gen.ts";
import {
  type BindingKind,
  type Blend,
  colorAttachmentBytes,
  dimensionOf,
  formatOf,
  isDepthBinding,
  isStorage,
  type Manifest,
  type RenderTarget,
  sameTarget,
  targetName,
} from "./manifest.ts";
import { buildPathProblem, pathProblem } from "./path.ts";
import {
  argumentsOf,
  type Binding,
  type Command,
  indicesOf,
  isDraw,
  onScreen,
  type OpcodeName,
  type Pass,
  type TexelRect,
  type TextureFormat,
  type TextureKind,
} from "./stream.ts";

/** The limits a program sees, in `wrela.limit`'s order (runtime/abi `Limits`). */
export type Limits = number[];

/** WebGPU's limits behind each limit a program sees, in `wrela.limit`'s order: the least of
 * them. The device is opened with the adapter's own value of each. */
export const LIMIT_SOURCES = [
  ["maxTextureDimension2D"],
  // Every buffer can be bound as storage.
  ["maxBufferSize", "maxStorageBufferBindingSize"],
  ["maxStorageBuffersPerShaderStage"],
  ["maxUniformBufferBindingSize"],
  ["maxComputeWorkgroupStorageSize"],
  ["maxComputeInvocationsPerWorkgroup"],
  ["maxComputeWorkgroupSizeX"],
  ["maxComputeWorkgroupSizeY"],
  ["maxComputeWorkgroupSizeZ"],
  ["maxComputeWorkgroupsPerDimension"],
] as const;

/** The device's limits, in `wrela.limit`'s order: at most `u32::MAX` each; then its features'
 * bits (`FEATURES`, as `features` names them) and its subgroups' least and greatest sizes. */
export function limitsOf(l: GPUSupportedLimits, features: readonly string[], subgroups: [number, number]): Limits {
  const bits = FEATURES.reduce((a, f, i) => (features.includes(f) ? a | (1 << i) : a), 0);
  return [...LIMIT_SOURCES.map((names) => Math.min(...names.map((n) => l[n]), 0xffff_ffff)), bits >>> 0, ...subgroups];
}

/** A well-formed command the host can't carry out. */
export class CommandError extends Error {
  constructor(
    readonly opcode: OpcodeName,
    why: string,
  ) {
    super(`${opcode} failed: ${why}`);
    this.name = "CommandError";
  }
}

/** That texture `handle`, of format `got`, is of the format its binding names. */
function sameFormat(err: (why: string) => Error, handle: number, got: TextureFormat, want: TextureFormat | null): void {
  if (want !== null && want !== got) throw err(`texture ${handle} is ${got}, and it's bound where a ${want} texture goes`);
}

/** How a shader reads a colour texture of format `f`: filtered floats, unfiltered floats, or
 * unsigned integers. A compressed texture exists only with its feature, so it always filters. */
export function sampleType(f: TextureFormat): GPUTextureSampleType {
  const t = formatOf(f);
  return t.uint ? "uint" : t.filterable ? "float" : "unfilterable-float";
}

/** Whether a texture format is a depth format. */
export const isDepth = (f: TextureFormat) => formatOf(f).depth;
/** The bytes of one block of a texture format: a texel, or a compressed format's 4 × 4. */
export const bytesPerTexel = (f: TextureFormat) => formatOf(f).bytes;
/** A block's side in texels: 4 for a compressed format, else 1. */
export const blockOf = (f: TextureFormat) => formatOf(f).block;
/** The bytes of a `width` × `height` rectangle of `f`, whole blocks. */
export const bytesOf = (f: TextureFormat, width: number, height: number) => {
  const b = blockOf(f);
  return Math.ceil(width / b) * Math.ceil(height / b) * bytesPerTexel(f);
};
/** The targets outside a pass: none. */
const NO_TARGET: RenderTarget = { colors: [], depth: false, screen: false };

/** The most mip levels a `width` × `height` texture has (runtime/abi `max_levels`). */
export const maxLevels = (width: number, height: number) => 32 - Math.clz32(Math.max(width, height, 1));
/** The width and height of level `level` of a `width` × `height` texture (runtime/abi
 * `level_size`). */
export const levelSize = (width: number, height: number, level: number): [number, number] => [Math.max(1, width >> level), Math.max(1, height >> level)];

/** The names of the features a device has, by the bits `wrela.limit(10)` gives. */
export const featureNames = (bits: number): string[] => FEATURES.filter((_, i) => (bits & (1 << i)) !== 0);

/** The rules a host keeps whether or not it checks commands against WebGPU's, which checks none
 * of them (a release build's normal run checks only these): a request stays inside the
 * program's storage, its build's directory beside its siblings, and its origin (runtime/abi
 * `path_problem` and `build_path_problem`); a timed label names something; and a multi-draw
 * has no count buffer, which a device without multi-draw couldn't honour. */
export function checkHostRules(cmd: Command): void {
  const err = (why: string) => new CommandError(cmd.op, why);
  switch (cmd.op) {
    case "StorageRead":
    case "StorageWrite": {
      const why = pathProblem(cmd.path);
      if (why !== undefined) throw err(`the storage path \`${cmd.path}\` ${why}`);
      return;
    }
    case "Fetch":
    case "Post": {
      const why = pathProblem(cmd.url);
      if (why !== undefined) throw err(`the URL \`${cmd.url}\` ${why}`);
      return;
    }
    case "Timed":
      if (cmd.label === "") throw err("a timed label is empty: it names no pass or dispatch");
      return;
    case "Prepare":
    case "Unprepare":
    case "HandOver": {
      const why = buildPathProblem(cmd.path);
      if (why !== undefined) throw err(`the build path \`${cmd.path}\` ${why}`);
      return;
    }
    case "HostRequest": {
      const why = pathProblem(cmd.service);
      if (why !== undefined) throw err(`the host service \`${cmd.service}\` ${why}`);
      return;
    }
    // A count the GPU writes would mean what a device without multi-draw can't do (the host
    // draws `count` draws one by one): not in 0.6. An unused draw has no instances instead.
    case "MultiDrawIndirect":
    case "MultiDrawIndexedIndirect":
      if (cmd.count_buffer !== NONE) throw err("a multi-draw's count buffer isn't supported: it draws `count` draws, so give the unused ones no instances");
      return;
  }
}

interface Shape {
  name: string;
  compute: boolean;
  uniformSize: number;
  /** Each binding's kind, and a colour texture's format. */
  bindings: { kind: BindingKind; format: TextureFormat | null }[];
  /** A render pipeline's targets: the passes it may be drawn in. */
  targets: RenderTarget[];
  /** How each colour output blends. */
  blends: Blend[];
  /** The device features its shader needs. */
  requires: string[];
}

/** A live texture. */
type Texture = {
  kind: "texture";
  width: number;
  height: number;
  textureKind: TextureKind;
  /** A 3D texture's depth, an array's layers (6 of a cube), 1 of a 2D texture. */
  layers: number;
  levels: number;
  format: TextureFormat;
  writable: boolean;
};

/** Its 2D layers: a 3D texture's whole depth is one subresource. */
const subresourceLayers = (t: Texture) => (t.textureKind === "3d" ? 1 : t.layers);

/** A live resource. */
type Resource = { kind: "buffer"; size: number } | Texture | { kind: "sampler"; comparison: boolean };

/** A use of a buffer, or of a texture's levels and layers, in a usage scope (WebGPU tracks each
 * subresource apart): `[from, to)` each. */
type Use = { levels: [number, number]; layers: [number, number]; write: boolean };
const BUFFER: [number, number] = [0, 1];
/** A buffer's use, read-only or read-write; no `Use` changes once made. */
const BUFFER_READ: Use = { levels: BUFFER, layers: BUFFER, write: false };
const BUFFER_WRITE: Use = { levels: BUFFER, layers: BUFFER, write: true };
const overlaps = (a: Use, b: Use) =>
  a.levels[0] < b.levels[1] && b.levels[0] < a.levels[1] && a.layers[0] < b.layers[1] && b.layers[0] < a.layers[1];
const sameUse = (a: Use, b: Use) =>
  a.write === b.write && a.levels[0] === b.levels[0] && a.levels[1] === b.levels[1] && a.layers[0] === b.layers[0] && a.layers[1] === b.layers[1];

/** A pass's attachment: a texture's level and layer. */
type Attachment = { handle: number; level: number; layer: number };

export class Checker {
  readonly #pipelines: Shape[];
  readonly #resources = new Map<number, Resource>();
  /** Original values of resource handles changed in the current export, if one is active. */
  #inCall = false;
  readonly #beforeCall = new Map<number, Resource | undefined>();
  /** The current usage scope, a dispatch or the open pass: each buffer's or texture's uses so
   * far. Dispatches happen only outside a pass (the `Sequencer` checks), so the two kinds of
   * scope never overlap and share this map. */
  #scope = new Map<number, Use[]>();
  /** An empty map for the next fresh scope; a rejected command restores the current one. */
  #spareScope = new Map<number, Use[]>();
  /** Each handle a draw may add uses to, and how many it had before (-1: no entry), reused and
   * cleared after each check, whether it passes or fails. A command only adds uses, so rollback
   * truncates. */
  readonly #beforeDraw = new Map<number, number>();
  /** Read-write subresources in this command, reused across draws and dispatches. */
  readonly #written: [number, Use][] = [];
  /** The open pass's attachments. */
  #attachments: Attachment[] = [];
  /** The open pass's targets: which pipelines its draws may draw with. */
  #targets: RenderTarget = NO_TARGET;
  /** The open pass's size, for its scissors. */
  #passSize: [number, number] | undefined;
  /** The current frame's screen size, when a frame has begun. */
  #screenSize: [number, number] | undefined;

  readonly #limits: Limits;
  readonly #features: string[];

  /** The device's limits, in `wrela.limit`'s order. */
  get limits(): Limits {
    return this.#limits;
  }

  /** A checker for a device with these limits (WebGPU's defaults unless given); its features are
   * the limits' tenth's bits. */
  constructor(manifest: Manifest, limits: Limits = DEFAULT_LIMITS) {
    this.#limits = limits;
    this.#features = featureNames(limits[10] ?? 0);
    this.#pipelines = manifest.pipelines.map((p) => ({
      name: p.name,
      compute: p.kind === "compute",
      uniformSize: p.uniform?.size ?? 0,
      bindings: p.bindings.map((b) => ({ kind: b.kind, format: b.format })),
      targets: p.kind === "render" ? p.targets : [],
      blends: p.kind === "render" ? p.blends : [],
      requires: p.requires,
    }));
  }

  #has(feature: string | null): boolean {
    return feature === null || this.#features.includes(feature);
  }

  /** Gives screen passes the frame's canvas size for attachment checks. */
  setScreenSize(width: number, height: number): void {
    this.#screenSize = [width, height];
  }

  /** Begins an export outside any pass. Resource changes remain reversible until it returns. */
  beginCall(): void {
    this.#inCall = true;
  }

  /** Commits a successful export, or forgets all its resource changes. An export never keeps
   * an open pass, so its usage scope and attachments need not cross this boundary. */
  endCall(commit: boolean): void {
    if (!commit) {
      for (const [handle, resource] of this.#beforeCall) {
        if (resource === undefined) this.#resources.delete(handle);
        else this.#resources.set(handle, resource);
      }
    }
    this.#beforeCall.clear();
    this.#inCall = false;
    this.#scope.clear();
    this.#spareScope.clear();
    this.#attachments = [];
    this.#targets = NO_TARGET;
    this.#passSize = undefined;
    this.#written.length = 0;
  }

  #remember(handle: number): void {
    if (this.#inCall && !this.#beforeCall.has(handle)) this.#beforeCall.set(handle, this.#resources.get(handle));
  }

  /** Checks a command that has passed the `Sequencer`, and records its effect. */
  check(cmd: Command): void {
    // A rejected command leaves no trace. Binding checks add uses as they go, and a later
    // binding (or an indirect range) may still reject the command.
    if (cmd.op === "Dispatch" || cmd.op === "DispatchIndirect" || cmd.op === "BeginPass") {
      // A fresh scope can use the empty spare; the old map needn't be copied to roll back.
      const scope = this.#scope;
      const attachments = this.#attachments;
      const targets = this.#targets;
      const size = this.#passSize;
      this.#scope = this.#spareScope;
      this.#spareScope = scope;
      try {
        this.#check(cmd);
      } catch (e) {
        this.#spareScope = this.#scope;
        this.#scope = scope;
        if (cmd.op === "BeginPass") {
          // BeginPass replaces these before adding attachments; the old values stay intact.
          this.#attachments = attachments;
          this.#targets = targets;
          this.#passSize = size;
        }
        throw e;
      } finally {
        this.#spareScope.clear();
      }
      return;
    }
    if (!isDraw(cmd)) {
      this.#check(cmd);
      return;
    }
    // A draw can only add uses to the entries it names, so keep only how many each had.
    const entries = this.#beforeDraw;
    for (const binding of cmd.bindings) this.#rememberUse(binding.handle);
    const args = argumentsOf(cmd);
    if (args !== null) this.#rememberUse(args);
    const indices = indicesOf(cmd);
    if (indices !== null) this.#rememberUse(indices);
    // A multi-draw's count buffer adds no use: a command with one is rejected.
    try {
      this.#check(cmd);
    } catch (e) {
      for (const [handle, n] of entries) {
        if (n < 0) this.#scope.delete(handle);
        else this.#scope.get(handle)!.length = n;
      }
      throw e;
    } finally {
      entries.clear();
    }
  }

  #rememberUse(handle: number): void {
    if (!this.#beforeDraw.has(handle)) this.#beforeDraw.set(handle, this.#scope.get(handle)?.length ?? -1);
  }

  #check(cmd: Command): void {
    const op = cmd.op;
    const err = (why: string) => new CommandError(op, why);
    switch (cmd.op) {
      case "CreateBuffer": {
        const { handle, size } = cmd;
        const max = this.#limits[1]!;
        if (size > max) throw err(`buffer ${handle} is ${size} bytes; the limit is ${max}`);
        this.#create(op, handle, { kind: "buffer", size });
        return;
      }
      case "CreateTexture": {
        const { handle, width, height, format, writable, kind, layers, levels } = cmd;
        const max = this.#limits[0]!;
        if (width > max || height > max) {
          throw err(`texture ${handle} is ${width}x${height}; the limit is ${max} a side`);
        }
        if (kind === "3d" && [width, height, layers].some((s) => s > MAX_TEXTURE_3D)) {
          throw err(`3D texture ${handle} is ${width}x${height}x${layers}; the limit is ${MAX_TEXTURE_3D} a side`);
        }
        if (kind === "2d-array" && layers > MAX_LAYERS) {
          throw err(`texture array ${handle} has ${layers} layers; the limit is ${MAX_LAYERS}`);
        }
        const most = maxLevels(width, height);
        if (levels > most) {
          throw err(`texture ${handle} is ${width}x${height}, which has at most ${most} mip levels, not ${levels}`);
        }
        const info = formatOf(format);
        if (info.block > 1) {
          if (!this.#has(info.needs.filter)) {
            throw err(`texture ${handle} is ${format}, which needs the device feature \`${info.needs.filter ?? ""}\``);
          }
          if (width % info.block !== 0 || height % info.block !== 0) {
            throw err(`texture ${handle} is ${format}, whose ${info.block}x${info.block} blocks a ${width}x${height} texture doesn't hold whole`);
          }
        }
        if (writable && !(info.storable && this.#has(info.needs.store))) {
          throw err(`texture ${handle} is made writable, and kernels can't write ${format} on this device`);
        }
        this.#create(op, handle, { kind: "texture", width, height, textureKind: kind, layers, levels, format, writable });
        return;
      }
      case "CreateSampler":
        this.#create(op, cmd.handle, { kind: "sampler", comparison: cmd.compare !== null });
        return;
      case "DestroyBuffer":
        this.#buffer(op, cmd.handle);
        this.#destroy(cmd.handle);
        return;
      case "DestroyTexture":
        this.#texture(op, cmd.handle);
        this.#destroy(cmd.handle);
        return;
      case "DestroySampler":
        this.#sampler(op, cmd.handle);
        this.#destroy(cmd.handle);
        return;
      case "WriteBuffer":
        this.#range(op, cmd.handle, cmd.offset, cmd.data.length, "writing");
        return;
      case "ReadBuffer":
        this.#range(op, cmd.handle, cmd.offset, cmd.size, "reading");
        return;
      case "CopyBuffer": {
        const { source, destination, size } = cmd;
        if (source === destination) throw err(`buffer ${source} is both the copy's source and its destination`);
        this.#range(op, source, cmd.sourceOffset, size, "copying");
        this.#range(op, destination, cmd.destinationOffset, size, "copying");
        return;
      }
      case "WriteTexture": {
        const { handle, width, height, data } = cmd;
        const t = this.#texture(op, handle);
        if (t.textureKind === "3d") throw err(`texture ${handle} is a 3D texture, whose texels kernels write`);
        if (isDepth(t.format)) throw err(`texture ${handle} is a depth texture, which only a pass can write`);
        this.#rect(op, t, { texture: handle, level: cmd.level, layer: cmd.layer, x: cmd.x, y: cmd.y, width, height }, "writing");
        const want = bytesOf(t.format, width, height);
        if (data.length !== want) {
          throw err(`${width}x${height} texels of ${t.format} are ${want} bytes, not ${data.length}`);
        }
        return;
      }
      case "CopyTexture": {
        const { source, destination } = cmd;
        const s = this.#texture(op, source.texture);
        const d = this.#texture(op, destination.texture);
        if (s.format !== d.format) {
          throw err(`texture ${source.texture} is ${s.format} and texture ${destination.texture} is ${d.format}: a copy is between textures of one format`);
        }
        if (isDepth(s.format) || s.textureKind === "3d" || d.textureKind === "3d") {
          throw err("a copy between textures is of 2D colour texels");
        }
        const at = { ...destination, width: source.width, height: source.height };
        this.#rect(op, s, source, "copying from");
        this.#rect(op, d, at, "copying to");
        if (source.texture === destination.texture && source.level === destination.level && source.layer === destination.layer) {
          throw err(`level ${source.level} and layer ${source.layer} of texture ${source.texture} is both the copy's source and its destination`);
        }
        return;
      }
      case "CopyTextureToBuffer": {
        const t = this.#texture(op, cmd.source.texture);
        if (t.textureKind === "3d") throw err("a texture copied to a buffer is a 2D one");
        const s = cmd.source;
        this.#rect(op, t, s, "copying from");
        // WebGPU copies a depth texture's texels only by whole levels.
        const [w, h] = levelSize(t.width, t.height, s.level);
        if (isDepth(t.format) && (s.x !== 0 || s.y !== 0 || s.width !== w || s.height !== h)) {
          throw err(`texture ${s.texture} is a depth texture, copied only whole: all ${w}x${h} texels of level ${s.level} from (0, 0)`);
        }
        this.#bufferRows(op, t.format, s, cmd.buffer, cmd.offset, cmd.bytesPerRow);
        return;
      }
      case "CopyBufferToTexture": {
        const t = this.#texture(op, cmd.destination.texture);
        if (t.textureKind === "3d" || isDepth(t.format)) throw err("a buffer is copied into a 2D colour texture");
        this.#rect(op, t, cmd.destination, "copying to");
        this.#bufferRows(op, t.format, cmd.destination, cmd.buffer, cmd.offset, cmd.bytesPerRow);
        return;
      }
      case "Dispatch": {
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        const max = this.#limits[9]!;
        if (cmd.groups.some((g) => g > max)) {
          throw err(`${cmd.groups.join("x")} workgroups is over the limit of ${max} per dimension`);
        }
        return;
      }
      case "DispatchIndirect":
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        this.#readRange(op, cmd.arguments, cmd.offset, 12, "reading arguments:");
        return;
      case "BeginScreenPass":
        this.#clearColour(op, cmd.clear);
        this.#scope.clear();
        this.#attachments = [];
        this.#targets = { colors: [SCREEN_FORMAT], depth: false, screen: true };
        this.#passSize = this.#screenSize;
        return;
      case "BeginPass":
        this.#beginPass(op, cmd.pass);
        return;
      case "SetScissor": {
        const size = this.#passSize;
        if (size !== undefined && (cmd.x + cmd.width > size[0] || cmd.y + cmd.height > size[1])) {
          throw err(`a scissor of ${cmd.width}x${cmd.height} at (${cmd.x}, ${cmd.y}) overruns the pass's ${size[0]}x${size[1]} targets`);
        }
        return;
      }
      case "SetViewport": {
        // WebGPU's bounds, compared in f32 as wgpu does: a side at most the largest texture's,
        // the edges within twice that of the origin.
        const [x, y, width, height] = cmd.rect;
        const max = this.#limits[0]!;
        if (width > Math.fround(max) || height > Math.fround(max)) throw err(`a viewport is at most ${max}x${max}, the device's largest texture`);
        const range = 2 * max;
        if (x < -Math.fround(range) || y < -Math.fround(range) || Math.fround(x + width) > Math.fround(range - 1) || Math.fround(y + height) > Math.fround(range - 1)) {
          throw err(`a viewport's edges are from -${range} to ${range - 1}, twice the device's largest texture`);
        }
        return;
      }
      case "Draw":
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        return;
      case "DrawIndirect":
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        this.#readRange(op, cmd.arguments, cmd.offset, 16, "reading arguments:");
        return;
      case "DrawIndexedIndirect":
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        this.#indices(op, cmd.indices, cmd.index_offset, cmd.index_size);
        this.#readRange(op, cmd.arguments, cmd.offset, 20, "reading arguments:");
        return;
      case "DrawIndexed": {
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        this.#indices(op, cmd.indices, cmd.index_offset, cmd.index_size);
        const last = cmd.first_index + cmd.index_count;
        if (last * 4 > cmd.index_size) {
          throw err(`indices ${cmd.first_index} to ${last} run past the ${Math.floor(cmd.index_size / 4)} bound`);
        }
        const lastInstance = cmd.first_instance + cmd.instances;
        if (lastInstance > 0xffff_ffff) {
          throw err(`instances ${cmd.first_instance} to ${lastInstance} run past ${0xffff_ffff}, the largest u32`);
        }
        return;
      }
      case "MultiDrawIndirect":
      case "MultiDrawIndexedIndirect": {
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        const stride = cmd.indices !== null ? 20 : 16;
        if (cmd.indices !== null) this.#indices(op, cmd.indices[0], cmd.indices[1], cmd.indices[2]);
        const bytes = cmd.count * stride;
        if (bytes > 0xffff_ffff) throw err(`${cmd.count} draws' arguments are more than a buffer holds`);
        this.#readRange(op, cmd.arguments, cmd.offset, bytes, "reading arguments:");
        checkHostRules(cmd);
        return;
      }
      case "Present":
      case "EndPass":
        this.#attachments = [];
        this.#passSize = undefined;
        return;
      case "StorageRead":
      case "StorageWrite":
      case "Fetch":
      case "Post":
      case "Timed":
      case "Prepare":
      case "Unprepare":
      case "HandOver":
      case "HostRequest":
        checkHostRules(cmd);
        return;
      case "Label":
        return;
    }
  }

  /** A pass's attachments (runtime/abi `Checker::begin_pass`). */
  #beginPass(op: OpcodeName, pass: Pass): void {
    const err = (why: string) => new CommandError(op, why);
    for (const c of pass.colors) this.#clearColour(op, c.clear);
    if (!Number.isFinite(pass.clearDepth)) throw err("the clear depth isn't a finite number");
    if (pass.depth !== NONE && !pass.keepDepth && !(pass.clearDepth >= 0 && pass.clearDepth <= 1)) {
      throw err("the clear depth isn't from 0 to 1");
    }
    this.#attachments = [];
    let size: [number, number] | undefined;
    const formats: TextureFormat[] = [];
    const screen = onScreen(pass);
    if (screen) {
      size = this.#screenSize;
      formats.push(SCREEN_FORMAT);
    }
    for (const c of pass.colors) {
      if (c.texture === SCREEN) continue;
      const t = this.#texture(op, c.texture);
      if (t.textureKind === "3d") throw err(`texture ${c.texture} is a 3D texture, so it can't be a target`);
      if (isDepth(t.format)) throw err(`texture ${c.texture} is a depth texture, so it can't be a colour target`);
      const info = formatOf(t.format);
      if (!(info.renderable && this.#has(info.needs.render))) {
        const why = info.needs.render !== null ? `drawing into ${t.format} needs the device feature \`${info.needs.render}\`` : `a pass can't draw into ${t.format}`;
        throw err(`texture ${c.texture}: ${why}`);
      }
      this.#subresource(op, t, c.texture, c.level, c.layer);
      const at = levelSize(t.width, t.height, c.level);
      if (size !== undefined && (size[0] !== at[0] || size[1] !== at[1])) throw err("the pass's targets differ in size");
      size = at;
      formats.push(t.format);
      if (this.#attachments.some((a) => a.handle === c.texture && a.level === c.level && a.layer === c.layer)) {
        throw err(`level ${c.level} and layer ${c.layer} of texture ${c.texture} is two of the pass's targets`);
      }
      this.#attachments.push({ handle: c.texture, level: c.level, layer: c.layer });
    }
    const bytes = colorAttachmentBytes(formats);
    if (bytes > MAX_COLOR_ATTACHMENT_BYTES) {
      throw err(`colour targets ${formats.join(", ")} take ${bytes} bytes a sample; WebGPU's limit is ${MAX_COLOR_ATTACHMENT_BYTES}`);
    }
    if (pass.depth !== NONE) {
      const t = this.#texture(op, pass.depth);
      if (!isDepth(t.format)) throw err(`texture ${pass.depth} isn't a depth texture`);
      this.#subresource(op, t, pass.depth, 0, pass.depthLayer);
      const at = levelSize(t.width, t.height, 0);
      if (size !== undefined && (size[0] !== at[0] || size[1] !== at[1])) throw err("the pass's colour and depth targets differ in size");
      size = at;
      this.#attachments.push({ handle: pass.depth, level: 0, layer: pass.depthLayer });
    }
    if (pass.colors.length === 0 && pass.depth === NONE) throw err("a pass needs a colour target or a depth target");
    this.#targets = { colors: formats, depth: pass.depth !== NONE, screen };
    this.#passSize = size;
  }

  /** That level `level` and layer `layer` of texture `handle` (`t`) exist. */
  #subresource(op: OpcodeName, t: Texture, handle: number, level: number, layer: number): void {
    if (level >= t.levels || layer >= subresourceLayers(t)) {
      throw new CommandError(op, `texture ${handle} has ${t.levels} levels and ${subresourceLayers(t)} layers: there's no level ${level} of layer ${layer}`);
    }
  }

  /** That `r` is inside its texture `t`'s level and layer, and whole blocks of a compressed one
   * (but at the level's edge). */
  #rect(op: OpcodeName, t: Texture, r: TexelRect, doing: string): void {
    const err = (why: string) => new CommandError(op, why);
    this.#subresource(op, t, r.texture, r.level, r.layer);
    const [w, h] = levelSize(t.width, t.height, r.level);
    const { x, y, width, height, texture: handle } = r;
    if (x + width > w || y + height > h) {
      throw err(`${doing} ${width}x${height} texels at (${x}, ${y}) overruns level ${r.level} of texture ${handle} (${w}x${h})`);
    }
    const b = blockOf(t.format);
    if (b > 1) {
      const whole = (at: number, n: number, side: number) => at % b === 0 && (n % b === 0 || at + n === side);
      if (!whole(x, width, w) || !whole(y, height, h)) {
        throw err(`${doing} ${width}x${height} texels at (${x}, ${y}) of texture ${handle} cuts its ${b}x${b} blocks`);
      }
    }
  }

  /** That a copy's rows of `r`'s texels fit in buffer `buffer` from `offset`, `bytesPerRow`
   * apart. */
  #bufferRows(op: OpcodeName, format: TextureFormat, r: TexelRect, buffer: number, offset: number, bytesPerRow: number): void {
    const b = blockOf(format);
    // WebGPU's rule: a copy's buffer offset is a whole number of the format's blocks.
    const blockBytes = bytesPerTexel(format);
    if (offset % blockBytes !== 0) throw new CommandError(op, `the buffer's offset ${offset} isn't a multiple of a ${format} block's ${blockBytes} bytes`);
    const row = Math.ceil(r.width / b) * bytesPerTexel(format);
    if (bytesPerRow < row) throw new CommandError(op, `a row of ${r.width} texels is ${row} bytes, more than ${bytesPerRow} a row`);
    const rows = Math.ceil(r.height / b);
    const bytes = rows === 0 ? 0 : (rows - 1) * bytesPerRow + row;
    this.#range(op, buffer, offset, Math.min(bytes, 0xffff_ffff), "copying");
  }

  /** An indexed draw's indices: a non-empty range of buffer `handle`, read. */
  #indices(op: OpcodeName, handle: number, offset: number, size: number): void {
    if (size === 0) throw new CommandError(op, `the index range of buffer ${handle} is empty`);
    this.#readRange(op, handle, offset, size, "reading indices:");
  }

  #clearColour(op: OpcodeName, clear: [number, number, number, number]): void {
    const i = clear.findIndex((c) => !Number.isFinite(c));
    if (i >= 0) throw new CommandError(op, `the clear colour's ${"rgba"[i]} isn't a finite number`);
  }

  #create(op: OpcodeName, handle: number, r: Resource): void {
    const old = this.#resources.get(handle);
    if (old !== undefined) throw new CommandError(op, `handle ${handle} already names a ${old.kind}`);
    this.#remember(handle);
    this.#resources.set(handle, r);
  }

  #destroy(handle: number): void {
    this.#remember(handle);
    this.#resources.delete(handle);
  }

  #buffer(op: OpcodeName, handle: number): number {
    const r = this.#resources.get(handle);
    if (r?.kind !== "buffer") throw new CommandError(op, `there's no buffer ${handle}`);
    return r.size;
  }

  #texture(op: OpcodeName, handle: number): Texture {
    const r = this.#resources.get(handle);
    if (r?.kind !== "texture") throw new CommandError(op, `there's no texture ${handle}`);
    return r;
  }

  #sampler(op: OpcodeName, handle: number): boolean {
    const r = this.#resources.get(handle);
    if (r?.kind !== "sampler") throw new CommandError(op, `there's no sampler ${handle}`);
    return r.comparison;
  }

  /** Checks that `size` bytes at `offset` fit in buffer `handle`. */
  #range(op: OpcodeName, handle: number, offset: number, size: number, doing: string): void {
    const total = this.#buffer(op, handle);
    if (offset + size > total) {
      throw new CommandError(op, `${doing} ${size} bytes at offset ${offset} overruns buffer ${handle} (${total} bytes)`);
    }
  }

  /** `size` bytes at `offset` in buffer `handle`, read (an indirect command's arguments, or
   * its indices). */
  #readRange(op: OpcodeName, handle: number, offset: number, size: number, doing: string): void {
    this.#range(op, handle, offset, size, doing);
    this.#used(op, "buffer", handle, BUFFER_READ);
  }

  /** Adds a use of a buffer or texture (`what`) to the usage scope. */
  #used(op: OpcodeName, what: string, handle: number, u: Use): void {
    let uses = this.#scope.get(handle);
    if (uses === undefined) this.#scope.set(handle, (uses = []));
    if (uses.some((o) => o.write !== u.write && overlaps(o, u))) {
      const scope = op === "Dispatch" || op === "DispatchIndirect" ? "dispatch" : "pass";
      throw new CommandError(op, `${what} ${handle} is used both read-only and read-write in one ${scope}`);
    }
    if (!uses.some((o) => sameUse(o, u))) uses.push(u);
  }

  /** Checks a dispatch's or draw's bindings, and adds what they use to the usage scope. */
  #bindings(op: OpcodeName, pipeline: number, bindings: Binding[], uniformLen: number): void {
    const err = (why: string) => new CommandError(op, why);
    const p = this.#pipelines[pipeline];
    if (p === undefined) {
      throw err(`there's no pipeline ${pipeline} (the manifest has ${this.#pipelines.length})`);
    }
    const name = p.name;
    const dispatch = op === "Dispatch" || op === "DispatchIndirect";
    if (p.compute !== dispatch) {
      const [is, needs] = p.compute ? ["compute", "render"] : ["render", "compute"];
      throw err(`pipeline ${pipeline} (${name}) is a ${is} pipeline; ${op} needs a ${needs} pipeline`);
    }
    const missing = p.requires.find((f) => !this.#features.includes(f));
    if (missing !== undefined) {
      throw err(`pipeline ${pipeline} (${name}) needs the device feature \`${missing}\`, which this device hasn't: check \`std::gpu::features()\` first`);
    }
    // A draw's pass's targets are one of its pipeline's.
    const here = this.#targets;
    if (!dispatch && !p.targets.some((t) => sameTarget(t, here))) {
      throw err(`\`${name}\` is drawn into ${targetName(here)}, which isn't one of its targets (${p.targets.map(targetName).join(", ")})`);
    }
    if (!dispatch) {
      for (const [c, f] of here.colors.entries()) {
        const blend = p.blends[c] ?? "replace";
        const info = formatOf(f);
        if (blend !== "replace" && !(info.blendable && this.#has(info.needs.blend))) {
          throw err(`\`${name}\` blends into ${f}, which needs the device feature \`${info.needs.blend ?? ""}\``);
        }
      }
    }
    if (bindings.length !== p.bindings.length) {
      throw err(
        `pipeline ${pipeline} (${name}) binds ${p.bindings.length} resources, but the command lists ${bindings.length}`,
      );
    }
    if (uniformLen !== p.uniformSize) {
      throw err(`pipeline ${pipeline} (${name}) takes ${p.uniformSize} uniform bytes, but the command has ${uniformLen}`);
    }
    const written = this.#written;
    written.length = 0;
    bindings.forEach((b, i) => {
      const { kind, format } = p.bindings[i]!;
      switch (kind) {
        case "read":
        case "read_write": {
          this.#range(op, b.handle, b.offset, b.size, "binding");
          if (b.size === 0 || b.size % 4 !== 0) {
            throw err(`a bound range of buffer ${b.handle} is ${b.size} bytes: a positive multiple of 4`);
          }
          if (b.offset % BINDING_OFFSET_ALIGNMENT !== 0) {
            throw err(
              `a bound range of buffer ${b.handle} starts at ${b.offset}, not a multiple of ${BINDING_OFFSET_ALIGNMENT} bytes`,
            );
          }
          const write = kind === "read_write";
          const u = write ? BUFFER_WRITE : BUFFER_READ;
          if (write) {
            if (written.some(([h]) => h === b.handle)) {
              throw err(`buffer ${b.handle} is bound read-write twice in one ${dispatch ? "dispatch" : "draw"}`);
            }
            written.push([b.handle, u]);
          }
          this.#used(op, "buffer", b.handle, u);
          return;
        }
        case "sampler":
        case "comparison_sampler":
        case "non_filtering_sampler": {
          if (b.offset !== 0 || b.size !== 0) {
            throw err(`a sampler binding has offset ${b.offset} and size ${b.size}: both must be 0`);
          }
          const comparison = this.#sampler(op, b.handle);
          if (comparison !== (kind === "comparison_sampler")) {
            throw err(`sampler ${b.handle} is bound where ${comparison ? "a filtering" : "a comparison"} sampler goes`);
          }
          return;
        }
        default: {
          const t = this.#texture(op, b.handle);
          const level = b.offset === 0 ? null : b.offset - 1;
          const layer = b.size === 0 ? null : b.size - 1;
          const write = isStorage(kind);
          if (write && !t.writable) throw err(`texture ${b.handle} is bound where a kernel writes it, but wasn't made writable`);
          const depth = isDepthBinding(kind);
          if (!write && isDepth(t.format) !== depth) {
            throw err(`texture ${b.handle} is bound where ${depth ? "a depth texture" : "a colour texture"} goes`);
          }
          sameFormat(err, b.handle, t.format, format);
          if (!takes(kind, t.textureKind, layer !== null)) {
            const shown = layer !== null ? `layer ${layer} of a ${t.textureKind} texture` : `a ${t.textureKind} texture`;
            throw err(`texture ${b.handle} is ${shown}, bound where a ${dimensionOf(kind) ?? "2d"} texture goes`);
          }
          if ((level !== null && level >= t.levels) || (layer !== null && layer >= subresourceLayers(t))) {
            const view = `View { level: ${level === null ? "None" : `Some(${level})`}, layer: ${layer === null ? "None" : `Some(${layer})`} }`;
            throw err(`texture ${b.handle} has ${t.levels} levels and ${subresourceLayers(t)} layers, and its binding's view is ${view}`);
          }
          // A storage view is one level: the one named, or the first.
          const levels: [number, number] = level !== null ? [level, level + 1] : write ? [0, 1] : [0, t.levels];
          const layers: [number, number] = layer !== null ? [layer, layer + 1] : [0, subresourceLayers(t)];
          const u: Use = { levels, layers, write };
          if (this.#attachments.some((a) => a.handle === b.handle && a.level >= levels[0] && a.level < levels[1] && a.layer >= layers[0] && a.layer < layers[1])) {
            throw err(`texture ${b.handle} is the pass's target, so its draws can't bind it`);
          }
          if (write) {
            if (written.some(([h, o]) => h === b.handle && overlaps(o, u))) {
              throw err(`texture ${b.handle} is bound for writing twice in one dispatch`);
            }
            written.push([b.handle, u]);
          }
          this.#used(op, "texture", b.handle, u);
          return;
        }
      }
    });
  }
}

/** The kinds of texture a binding of `kind` may be given (runtime/abi `BindingKind::takes`): a 2D
 * view of any texture but a 3D one (one layer of an array or a cube), an array's or a cube's whole
 * layers, a 3D one. */
export function takes(kind: BindingKind, textureKind: TextureKind, oneLayer: boolean): boolean {
  switch (dimensionOf(kind)) {
    case "2d":
      return textureKind === "2d" || (oneLayer && textureKind !== "3d");
    case "2d-array":
      return !oneLayer && (textureKind === "2d-array" || textureKind === "cube");
    case "cube":
      return !oneLayer && textureKind === "cube";
    case "3d":
      return textureKind === "3d";
    default:
      return false;
  }
}
