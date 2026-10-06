// Checks each command against the manifest and the program's resources before it runs. Mirrors
// runtime/abi/src/check.rs (same rules, same messages, checked by the ABI's test vectors), so
// both hosts reject the same programs at the command that's wrong, rather than with an
// asynchronous WebGPU error that names no command.

import { BINDING_OFFSET_ALIGNMENT, DEFAULT_LIMITS, NONE, SCREEN, TEXTURE_FORMATS } from "./abi.gen.ts";
import type { BindingKind, Manifest } from "./manifest.ts";
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

/**
 * What's wrong with a storage path or a fetched URL, if anything: it's relative to the program's
 * storage (or its build), with `/` between its parts, and none of them empty, `.` or `..`.
 */
export function pathProblem(path: string): string | undefined {
  if (path === "") return "is empty";
  if (path.startsWith("/") || /[\\:?#]/.test(path)) return "must be relative, with `/` between its parts";
  if (path.split("/").some((p) => p === "" || p === "." || p === "..")) return "has an empty, `.` or `..` part";
  return undefined;
}

const formatOf = (f: TextureFormat) => TEXTURE_FORMATS.find((t) => t.name === f)!;
/** Whether a texture format is a depth format. */
export const isDepth = (f: TextureFormat) => formatOf(f).depth;
const bytesPerTexel = (f: TextureFormat) => formatOf(f).bytes;

interface Shape {
  name: string;
  compute: boolean;
  uniformSize: number;
  bindings: BindingKind[];
}

/** A live resource. */
type Resource =
  | { kind: "buffer"; size: number }
  | { kind: "texture"; width: number; height: number; format: TextureFormat }
  | { kind: "sampler"; comparison: boolean };

export class Checker {
  readonly #pipelines: Shape[];
  readonly #resources = new Map<number, Resource>();
  /** The current usage scope, a dispatch or the open pass: each buffer's or texture's use so
   * far, [read-only, read-write]. Dispatches happen only outside a pass (the `Sequencer`
   * checks), so the two kinds of scope never overlap and share this map. */
  readonly #scope = new Map<number, [boolean, boolean]>();
  /** The open pass's attachments. */
  #attachments: number[] = [];

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
      bindings: p.bindings.map((b) => b.kind),
    }));
  }

  /** Checks a command that has passed the `Sequencer`, and records its effect. */
  check(cmd: Command): void {
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
        const { handle, width, height, format } = cmd;
        const max = this.#limits[0]!;
        if (width > max || height > max) {
          throw err(`texture ${handle} is ${width}x${height}; the limit is ${max} a side`);
        }
        this.#create(op, handle, { kind: "texture", width, height, format });
        return;
      }
      case "CreateSampler":
        this.#create(op, cmd.handle, { kind: "sampler", comparison: cmd.compare !== null });
        return;
      case "DestroyBuffer":
        this.#buffer(op, cmd.handle);
        this.#resources.delete(cmd.handle);
        return;
      case "DestroyTexture":
        this.#texture(op, cmd.handle);
        this.#resources.delete(cmd.handle);
        return;
      case "DestroySampler":
        this.#sampler(op, cmd.handle);
        this.#resources.delete(cmd.handle);
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
        this.#scope.clear();
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        const max = this.#limits[9]!;
        if (cmd.groups.some((g) => g > max)) {
          throw err(`${cmd.groups.join("x")} workgroups is over the limit of ${max} per dimension`);
        }
        return;
      }
      case "DispatchIndirect":
        this.#scope.clear();
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        this.#arguments(op, cmd.arguments, cmd.offset, 12);
        return;
      case "BeginScreenPass":
        this.#clearColour(op, cmd.clear);
        this.#scope.clear();
        this.#attachments = [];
        return;
      case "BeginPass": {
        const pass = cmd.pass;
        this.#clearColour(op, pass.clear);
        if (!Number.isFinite(pass.clearDepth)) throw err("the clear depth isn't a finite number");
        this.#scope.clear();
        this.#attachments = [];
        let size: [number, number] | undefined;
        if (pass.color !== SCREEN && pass.color !== NONE) {
          const t = this.#texture(op, pass.color);
          if (isDepth(t.format)) throw err(`texture ${pass.color} is a depth texture, so it can't be a colour target`);
          size = [t.width, t.height];
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
        return;
      }
      case "Draw":
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        return;
      case "DrawIndirect":
        this.#bindings(op, cmd.pipeline, cmd.bindings, cmd.uniforms.length);
        this.#arguments(op, cmd.arguments, cmd.offset, 16);
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
      case "Log":
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
    this.#resources.set(handle, r);
  }

  #buffer(op: OpcodeName, handle: number): number {
    const r = this.#resources.get(handle);
    if (r?.kind !== "buffer") throw new CommandError(op, `there's no buffer ${handle}`);
    return r.size;
  }

  #texture(op: OpcodeName, handle: number): { width: number; height: number; format: TextureFormat } {
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

  /** An indirect command's arguments: `size` bytes at `offset` in buffer `handle`, read. */
  #arguments(op: OpcodeName, handle: number, offset: number, size: number): void {
    this.#range(op, handle, offset, size, "reading arguments:");
    this.#used(op, "buffer", handle, false);
  }

  /** Adds a use of a buffer or texture (`what`) to the usage scope. */
  #used(op: OpcodeName, what: string, handle: number, write: boolean): void {
    const seen = this.#scope.get(handle) ?? [false, false];
    seen[write ? 1 : 0] = true;
    this.#scope.set(handle, seen);
    if (seen[0] && seen[1]) {
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
    if (bindings.length !== p.bindings.length) {
      throw err(
        `pipeline ${pipeline} (${name}) binds ${p.bindings.length} resources, but the command lists ${bindings.length}`,
      );
    }
    if (uniformLen !== p.uniformSize) {
      throw err(`pipeline ${pipeline} (${name}) takes ${p.uniformSize} uniform bytes, but the command has ${uniformLen}`);
    }
    const written: number[] = [];
    bindings.forEach((b, i) => {
      const kind = p.bindings[i]!;
      if (this.#attachments.includes(b.handle)) {
        throw err(`texture ${b.handle} is the pass's target, so its draws can't bind it`);
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
        case "depth_texture": {
          const t = this.#texture(op, b.handle);
          const depth = kind === "depth_texture";
          if (isDepth(t.format) !== depth) {
            throw err(`texture ${b.handle} is bound where ${depth ? "a depth texture" : "a colour texture"} goes`);
          }
          this.#used(op, "texture", b.handle, false);
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
