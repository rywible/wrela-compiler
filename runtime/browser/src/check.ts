// Checks each command against the manifest and the program's resources before it runs. Mirrors
// runtime/abi/src/check.rs (same rules, same messages, checked by the ABI's test vectors), so
// both hosts reject the same programs at the command that's wrong, rather than with an
// asynchronous WebGPU error that names no command.

import { BINDING_OFFSET_ALIGNMENT, DEFAULT_LIMITS, MAX_TEXTURE_3D, NONE, SCREEN, SCREEN_FORMAT, TEXTURE_FORMATS } from "./abi.gen.ts";
import { type BindingKind, is3d, isStorage, type Manifest, type RenderTarget, targetName } from "./manifest.ts";
import { buildPathProblem, pathProblem } from "./path.ts";
import type { Binding, Command, OpcodeName, TextureFormat } from "./stream.ts";

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

/** The device's limits, in `wrela.limit`'s order: at most `u32::MAX` each. */
export function limitsOf(l: GPUSupportedLimits): Limits {
  return LIMIT_SOURCES.map((names) => Math.min(...names.map((n) => l[n]), 0xffff_ffff));
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

const formatOf = (f: TextureFormat) => TEXTURE_FORMATS.find((t) => t.name === f)!;
/** That texture `handle`, of format `got`, is of the format its binding names. */
function sameFormat(err: (why: string) => Error, handle: number, got: TextureFormat, want: TextureFormat | null): void {
  if (want !== null && want !== got) throw err(`texture ${handle} is ${got}, and it's bound where a ${want} texture goes`);
}

/** How a shader reads a colour texture of format `f`: filtered floats, unfiltered floats, or
 * unsigned integers. */
export function sampleType(f: TextureFormat): GPUTextureSampleType {
  const t = formatOf(f);
  return t.uint ? "uint" : t.filterable ? "float" : "unfilterable-float";
}

/** Whether a texture format is a depth format. */
export const isDepth = (f: TextureFormat) => formatOf(f).depth;
/** The bytes of one texel of a texture format. */
export const bytesPerTexel = (f: TextureFormat) => formatOf(f).bytes;

interface Shape {
  name: string;
  compute: boolean;
  uniformSize: number;
  /** Each binding's kind, and a colour texture's format. */
  bindings: { kind: BindingKind; format: TextureFormat | null }[];
  /** A render pipeline's targets: the passes it may be drawn in. */
  targets: RenderTarget[];
}

/** A live texture: `depth` is a 3D texture's, or 0 for a 2D one. */
type Texture = { kind: "texture"; width: number; height: number; depth: number; format: TextureFormat; writable: boolean };

/** A live resource. */
type Resource = { kind: "buffer"; size: number } | Texture | { kind: "sampler"; comparison: boolean };

export class Checker {
  readonly #pipelines: Shape[];
  readonly #resources = new Map<number, Resource>();
  /** Original values of resource handles changed in the current export, if one is active. */
  #inCall = false;
  readonly #beforeCall = new Map<number, Resource | undefined>();
  /** The current usage scope, a dispatch or the open pass: each buffer's or texture's use so
   * far, bit 1 read-only and bit 2 read-write. Dispatches happen only outside a pass (the `Sequencer`
   * checks), so the two kinds of scope never overlap and share this map. */
  #scope = new Map<number, number>();
  /** An empty map for the next fresh scope; a rejected command restores the current one. */
  #spareScope = new Map<number, number>();
  /** Read-write buffers in this command, reused across draws and dispatches. */
  readonly #written: number[] = [];
  /** The open pass's attachments. */
  #attachments: number[] = [];
  /** The open pass's colour target's format (the screen's, a texture's, or none), and whether it
   * has a depth target: which pipelines its draws may draw with. */
  #targets: RenderTarget = { color: null, depth: false };
  /** The current frame's screen size, when a frame has begun. */
  #screenSize: [number, number] | undefined;

  readonly #limits: Limits;

  /** The device's limits, in `wrela.limit`'s order. */
  get limits(): Limits {
    return this.#limits;
  }

  /** A checker for a device with these limits (WebGPU's defaults unless given). */
  constructor(manifest: Manifest, limits: Limits = DEFAULT_LIMITS) {
    this.#limits = limits;
    this.#pipelines = manifest.pipelines.map((p) => ({
      name: p.name,
      compute: p.kind === "compute",
      uniformSize: p.uniform?.size ?? 0,
      bindings: p.bindings.map((b) => ({ kind: b.kind, format: b.format })),
      targets: p.kind === "render" ? p.targets : [],
    }));
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
    this.#targets = { color: null, depth: false };
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
        }
        throw e;
      } finally {
        this.#spareScope.clear();
      }
      return;
    }
    if (cmd.op !== "Draw" && cmd.op !== "DrawIndirect" && cmd.op !== "DrawIndexedIndirect") {
      this.#check(cmd);
      return;
    }
    // A draw can only change the entries it names, so keep only those prior uses.
    const entries = new Map<number, number | undefined>();
    const remember = (handle: number) => {
      if (entries.has(handle)) return;
      entries.set(handle, this.#scope.get(handle));
    };
    for (const binding of cmd.bindings) remember(binding.handle);
    if (cmd.op !== "Draw") remember(cmd.arguments);
    if (cmd.op === "DrawIndexedIndirect") remember(cmd.indices);
    try {
      this.#check(cmd);
    } catch (e) {
      for (const [handle, used] of entries) {
        if (used === undefined) this.#scope.delete(handle);
        else this.#scope.set(handle, used);
      }
      throw e;
    }
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
        const { handle, width, height, format, writable, depth } = cmd;
        const max = this.#limits[0]!;
        if (width > max || height > max) {
          throw err(`texture ${handle} is ${width}x${height}; the limit is ${max} a side`);
        }
        if (depth > 0 && [width, height, depth].some((s) => s > MAX_TEXTURE_3D)) {
          throw err(`3D texture ${handle} is ${width}x${height}x${depth}; the limit is ${MAX_TEXTURE_3D} a side`);
        }
        this.#create(op, handle, { kind: "texture", width, height, depth, format, writable });
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
        const { handle, x, y, width, height, data } = cmd;
        const t = this.#texture(op, handle);
        if (t.depth > 0) throw err(`texture ${handle} is a 3D texture, whose texels kernels write`);
        if (x + width > t.width || y + height > t.height) {
          throw err(`writing ${width}x${height} texels at (${x}, ${y}) overruns texture ${handle} (${t.width}x${t.height})`);
        }
        if (isDepth(t.format)) throw err(`texture ${handle} is a depth texture, which only a pass can write`);
        const want = width * height * bytesPerTexel(t.format);
        if (data.length !== want) {
          throw err(`${width}x${height} texels of ${t.format} are ${want} bytes, not ${data.length}`);
        }
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
        this.#arguments(op, cmd.arguments, cmd.offset, 12);
        return;
      case "BeginScreenPass":
        this.#clearColour(op, cmd.clear);
        this.#scope.clear();
        this.#attachments = [];
        this.#targets = { color: SCREEN_FORMAT, depth: false };
        return;
      case "BeginPass": {
        const pass = cmd.pass;
        this.#clearColour(op, pass.clear);
        if (!Number.isFinite(pass.clearDepth)) throw err("the clear depth isn't a finite number");
        if (pass.depth !== NONE && !pass.keepDepth && !(pass.clearDepth >= 0 && pass.clearDepth <= 1)) {
          throw err("the clear depth isn't from 0 to 1");
        }
        this.#attachments = [];
        for (const t of [pass.color, pass.depth]) {
          const r = this.#resources.get(t);
          if (t !== SCREEN && t !== NONE && r?.kind === "texture" && r.depth > 0) {
            throw err(`texture ${t} is a 3D texture, so it can't be a target`);
          }
        }
        let size: [number, number] | undefined = pass.color === SCREEN ? this.#screenSize : undefined;
        let colour: TextureFormat | null = pass.color === SCREEN ? SCREEN_FORMAT : null;
        if (pass.color !== SCREEN && pass.color !== NONE) {
          const t = this.#texture(op, pass.color);
          if (isDepth(t.format)) throw err(`texture ${pass.color} is a depth texture, so it can't be a colour target`);
          size = [t.width, t.height];
          colour = t.format;
          this.#attachments.push(pass.color);
        }
        if (pass.depth !== NONE) {
          const t = this.#texture(op, pass.depth);
          if (!isDepth(t.format)) throw err(`texture ${pass.depth} isn't a depth texture`);
          if (size !== undefined && (size[0] !== t.width || size[1] !== t.height)) {
            throw err("the pass's colour and depth targets differ in size");
          }
          this.#attachments.push(pass.depth);
        }
        if (pass.color === NONE && pass.depth === NONE) throw err("a pass needs a colour target or a depth target");
        this.#targets = { color: colour, depth: pass.depth !== NONE };
        return;
      }
      case "Draw":
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        return;
      case "DrawIndirect":
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        this.#arguments(op, cmd.arguments, cmd.offset, 16);
        return;
      case "DrawIndexedIndirect":
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        if (cmd.index_size === 0) throw err(`the index range of buffer ${cmd.indices} is empty`);
        this.#range(op, cmd.indices, cmd.index_offset, cmd.index_size, "reading indices:");
        this.#used(op, "buffer", cmd.indices, false);
        this.#arguments(op, cmd.arguments, cmd.offset, 20);
        return;
      case "Present":
      case "EndPass":
        this.#attachments = [];
        return;
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
      case "Label":
        return;
    }
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

  /** That texture `handle`, 3D or not (`is`), is 3D where the binding is (`three`), and 2D where
   * it isn't. */
  #dimensions(op: OpcodeName, handle: number, is: boolean, three: boolean): void {
    if (is === three) return;
    const [what, want] = is ? ["a 3D", "a 2D"] : ["a 2D", "a 3D"];
    throw new CommandError(op, `texture ${handle} is ${what} texture, bound where ${want} texture goes`);
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

  /** An indirect command's arguments: `size` bytes at `offset` in buffer `handle`, read. */
  #arguments(op: OpcodeName, handle: number, offset: number, size: number): void {
    this.#range(op, handle, offset, size, "reading arguments:");
    this.#used(op, "buffer", handle, false);
  }

  /** Adds a use of a buffer or texture (`what`) to the usage scope. */
  #used(op: OpcodeName, what: string, handle: number, write: boolean): void {
    const seen = (this.#scope.get(handle) ?? 0) | (write ? 2 : 1);
    this.#scope.set(handle, seen);
    if (seen === 3) {
      const scope = op === "Dispatch" || op === "DispatchIndirect" ? "dispatch" : "pass";
      throw new CommandError(op, `${what} ${handle} is used both read-only and read-write in one ${scope}`);
    }
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
    // A draw's pass's targets are one of its pipeline's.
    if (!dispatch) {
      const here = this.#targets;
      if (!p.targets.some((t) => t.color === here.color && t.depth === here.depth)) {
        throw err(`\`${name}\` is drawn into ${targetName(here)}, which isn't one of its targets (${p.targets.map(targetName).join(", ")})`);
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
      if (this.#attachments.includes(b.handle)) {
        throw err(`texture ${b.handle} is the pass's target, so its draws can't bind it`);
      }
      if (kind !== "read" && kind !== "read_write" && (b.offset !== 0 || b.size !== 0)) {
        throw err(`a texture or sampler binding has offset ${b.offset} and size ${b.size}: both must be 0`);
      }
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
          if (write) {
            if (written.includes(b.handle)) {
              throw err(`buffer ${b.handle} is bound read-write twice in one ${dispatch ? "dispatch" : "draw"}`);
            }
            written.push(b.handle);
          }
          this.#used(op, "buffer", b.handle, write);
          return;
        }
        case "texture":
        case "depth_texture":
        case "texture_3d":
        case "storage_texture":
        case "storage_texture_3d": {
          const t = this.#texture(op, b.handle);
          const write = isStorage(kind);
          if (write && !t.writable) throw err(`texture ${b.handle} is bound where a kernel writes it, but wasn't made writable`);
          const depth = kind === "depth_texture";
          if (!write && isDepth(t.format) !== depth) {
            throw err(`texture ${b.handle} is bound where ${depth ? "a depth texture" : "a colour texture"} goes`);
          }
          sameFormat(err, b.handle, t.format, format);
          this.#dimensions(op, b.handle, t.depth > 0, is3d(kind));
          this.#used(op, "texture", b.handle, write);
          return;
        }
        case "sampler":
        case "comparison_sampler": {
          const comparison = this.#sampler(op, b.handle);
          if (comparison !== (kind === "comparison_sampler")) {
            throw err(`sampler ${b.handle} is bound where ${comparison ? "a filtering" : "a comparison"} sampler goes`);
          }
          return;
        }
      }
    });
  }
}
