// The GPU side: builds the manifest's pipelines, and carries out decoded, sequenced and checked
// commands with WebGPU. The same mapping as the native host (runtime/native/src/gpu.rs):
//
// - Work is recorded into one pending command encoder and submitted ("flushed") at the end of a
//   pass on the screen, before a WriteTexture or a WriteBuffer over UPLOAD_MAX bytes (so earlier
//   work sees the old contents), before a readback, and at the end of each call. Each dispatch
//   gets its own compute pass.
// - Any other WriteBuffer is staged in an upload ring, written just before the flush, and a copy
//   from there is recorded where the write is: it splits no work into submissions (as the native
//   host's upload `Ring`).
// - A pass's draws are collected and recorded into one render pass when it ends (Present or
//   EndPass), since a pass can span batches.
// - A render pipeline is made at load for each of its manifest's `targets` (the passes the
//   program draws it in): the screen's or a texture's colour format, and a depth format or none.
//   With depth, a fragment is kept where its depth is less than what's there, which it replaces.
// - Uniform bytes go into a ring buffer bound with dynamic offsets (256-byte aligned by
//   default), staged on the CPU and written just before each flush, so every command in a
//   submission has its own slice.
// - Both rings grow (after a flush) when a submission needs more.
// - Bind group 0 of each pipeline is laid out from the manifest: the uniform block (dynamic
//   offset), then the bindings in order. Bind groups are cached per (pipeline, bindings), until
//   one of their resources is destroyed.
// - A destroyed buffer or texture is released after the next flush: work recorded before it
//   still uses it.

import { NONE, SCREEN, SCREEN_FORMAT, UPLOAD_MAX, UPLOAD_START } from "./abi.gen.ts";
import { bytesPerTexel, CommandError, isDepth, sampleType } from "./check.ts";
import { errorMessage } from "./errors.ts";
import { StateHash } from "./hash.ts";
import { is3d, type Manifest, type Pipeline, type ResourceBinding, type UniformBlock } from "./manifest.ts";
import { type Binding, type Bytes, type Command, joins, type OpcodeName, type Pass, type TextureFormat } from "./stream.ts";

const RING_START = 64 * 1024;

/** A pipeline failed to build: its WGSL didn't compile, or its layout or entry points are wrong. */
export class ShaderError extends Error {
  constructor(
    readonly pipeline: string,
    readonly shader: string,
    message: string,
  ) {
    super(`pipeline \`${pipeline}\` (shader ${shader}) failed to build:\n${message}`);
    this.name = "ShaderError";
  }
}

/** What a render pipeline needs to make a variant for other targets. */
interface RenderParts {
  module: GPUShaderModule;
  layout: GPUPipelineLayout;
  vertex: string;
  fragment: string;
  /** Its colour is drawn over the target's (the manifest's `blend`). */
  blend: boolean;
  /** The manifest's `cull` and `depth_bias`. */
  cull: GPUCullMode;
  depthBias: { constant: number; slope_scale: number; clamp: number };
  /** The manifest's `depth`: how fragments' depths are tested, and whether they're kept. */
  depth: { compare: GPUCompareFunction; write: boolean };
  /** By target formats: `colour|depth`, each a format or `none`. */
  variants: Map<string, GPURenderPipeline>;
}

export interface BuiltPipeline {
  name: string;
  layout: GPUBindGroupLayout;
  compute: GPUComputePipeline | null;
  render: RenderParts | null;
  uniform: UniformBlock | null;
  bindings: ResourceBinding[];
  /** A debug build's: where its bounds checks' flag is bound. */
  debugFlag: number | null;
}

/** Where passes on the screen draw. */
export interface ScreenTarget {
  /** The texture this frame's screen pass renders into (`SCREEN_FORMAT`). */
  texture(): GPUTexture;
  /** Called after a screen pass is recorded, with its encoder, before it's submitted. */
  afterPass?(encoder: GPUCommandEncoder): void;
}

function compilationMessages(info: GPUCompilationInfo): string | null {
  const errors = info.messages.filter((m) => m.type === "error");
  if (errors.length === 0) return null;
  return errors.map((m) => `${m.lineNum}:${m.linePos}: ${m.message}`).join("\n");
}

/** The key of a render pipeline's variant for these target formats. */
const targetsKey = (color: GPUTextureFormat | null, depth: GPUTextureFormat | null) => `${color ?? "none"}|${depth ?? "none"}`;

/** Straight alpha, over what's there (the manifest's `blend`). */
const OVER: GPUBlendState = {
  color: { srcFactor: "src-alpha", dstFactor: "one-minus-src-alpha", operation: "add" },
  alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
};

function renderDescriptor(
  name: string,
  parts: RenderParts,
  color: GPUTextureFormat | null,
  depth: GPUTextureFormat | null,
): GPURenderPipelineDescriptor {
  return {
    label: name,
    layout: parts.layout,
    vertex: { module: parts.module, entryPoint: parts.vertex, buffers: [] },
    // Triangle list, counter-clockwise front faces (the default).
    primitive: { topology: "triangle-list", cullMode: parts.cull },
    ...(depth === null
      ? {}
      : {
          depthStencil: {
            format: depth,
            depthWriteEnabled: parts.depth.write,
            depthCompare: parts.depth.compare,
            depthBias: parts.depthBias.constant,
            depthBiasSlopeScale: parts.depthBias.slope_scale,
            depthBiasClamp: parts.depthBias.clamp,
          },
        }),
    fragment: {
      module: parts.module,
      entryPoint: parts.fragment,
      targets: color === null ? [] : [{ format: color, ...(parts.blend ? { blend: OVER } : {}) }],
    },
  };
}

/** A colour texture binding's format, which the manifest's parser requires of it. */
function colourFormat(b: ResourceBinding): TextureFormat {
  if (b.format === null) throw new Error(`host bug: colour texture binding ${b.binding} has no format`);
  return b.format;
}

async function buildPipeline(device: GPUDevice, p: Pipeline, source: string): Promise<BuiltPipeline> {
  const fail = (message: string) => new ShaderError(p.name, p.shader, message);
  const visibility =
    p.kind === "compute" ? GPUShaderStage.COMPUTE : GPUShaderStage.VERTEX | GPUShaderStage.FRAGMENT;
  const entries: GPUBindGroupLayoutEntry[] = [];
  if (p.uniform !== null) {
    const u = p.uniform;
    const limit =
      u.space === "uniform" ? device.limits.maxUniformBufferBindingSize : device.limits.maxStorageBufferBindingSize;
    if (u.size > limit) throw fail(`its uniform block is ${u.size} bytes; the limit is ${limit}`);
    entries.push({
      binding: u.binding,
      visibility,
      buffer: {
        type: u.space === "uniform" ? "uniform" : "read-only-storage",
        hasDynamicOffset: true,
        minBindingSize: u.size,
      },
    });
  }
  if (p.debug_flag !== null) {
    // WebGPU forbids writable storage in vertex shaders (which aren't checked).
    entries.push({
      binding: p.debug_flag,
      visibility: p.kind === "compute" ? visibility : GPUShaderStage.FRAGMENT,
      buffer: { type: "storage" },
    });
  }
  for (const b of p.bindings) {
    // A render pipeline's binding one shader reads is that shader's alone, so each stage counts
    // only its own against WebGPU's limits.
    const seen =
      b.stage === "vertex" ? GPUShaderStage.VERTEX : b.stage === "fragment" ? GPUShaderStage.FRAGMENT : visibility;
    switch (b.kind) {
      case "read":
      case "read_write": {
        const readOnly = b.kind === "read";
        entries.push({
          binding: b.binding,
          // WebGPU forbids writable storage in vertex shaders.
          visibility: readOnly || p.kind === "compute" ? seen : GPUShaderStage.FRAGMENT,
          buffer: { type: readOnly ? "read-only-storage" : "storage" },
        });
        break;
      }
      case "texture":
      case "depth_texture":
      case "texture_3d":
        entries.push({
          binding: b.binding,
          visibility: seen,
          texture: {
            sampleType: b.kind === "depth_texture" ? "depth" : sampleType(colourFormat(b)),
            viewDimension: is3d(b.kind) ? "3d" : "2d",
          },
        });
        break;
      case "sampler":
      case "comparison_sampler":
        entries.push({
          binding: b.binding,
          visibility: seen,
          sampler: { type: b.kind === "sampler" ? "filtering" : "comparison" },
        });
        break;
      // Only a kernel writes a texture's texels.
      case "storage_texture":
      case "storage_texture_3d":
        entries.push({
          binding: b.binding,
          visibility: GPUShaderStage.COMPUTE,
          storageTexture: {
            access: "write-only",
            format: colourFormat(b),
            viewDimension: is3d(b.kind) ? "3d" : "2d",
          },
        });
        break;
    }
  }

  // Pipelines build at the same time and share the device's error scope stack, so the scope is
  // popped before anything awaits. The async creations below report through their promises.
  device.pushErrorScope("validation");
  const layout = device.createBindGroupLayout({ label: p.name, entries });
  const pipelineLayout = device.createPipelineLayout({ label: p.name, bindGroupLayouts: [layout] });
  const module = device.createShaderModule({ label: p.shader, code: source });
  const scope = device.popErrorScope();
  const compiled = compilationMessages(await module.getCompilationInfo());
  if (compiled !== null) throw fail(compiled);
  const error = await scope;
  if (error !== null) throw fail(error.message);
  let compute: GPUComputePipeline | null = null;
  let render: RenderParts | null = null;
  try {
    if (p.kind === "compute") {
      compute = await device.createComputePipelineAsync({
        label: p.name,
        layout: pipelineLayout,
        compute: { module, entryPoint: p.entry },
      });
    } else {
      render = {
        module,
        layout: pipelineLayout,
        vertex: p.vertex_entry,
        fragment: p.fragment_entry,
        blend: p.blend,
        cull: p.cull,
        depthBias: p.depth_bias,
        depth: p.depth,
        variants: new Map(),
      };
      // A variant for each of its targets (the passes the program draws it in, which the
      // compiler knows), side by side: a shader's errors come at load, and no frame waits to
      // compile one.
      const targets = p.targets.map((t): [GPUTextureFormat | null, GPUTextureFormat | null] => [
        t.color,
        t.depth ? "depth32float" : null,
      ]);
      const made = await Promise.all(
        targets.map(([c, d]) => device.createRenderPipelineAsync(renderDescriptor(p.name, render!, c, d))),
      );
      targets.forEach(([c, d], i) => render!.variants.set(targetsKey(c, d), made[i]!));
    }
  } catch (e) {
    throw fail(errorMessage(e));
  }
  return { name: p.name, layout, compute, render, uniform: p.uniform, bindings: p.bindings, debugFlag: p.debug_flag };
}

/** How long building each pipeline of `manifest` alone took (ms), one after another: where a
 * build's time to create its pipelines goes (test mode's `salt`). */
export async function timeEachPipeline(
  device: GPUDevice,
  manifest: Manifest,
  shaders: string[],
): Promise<{ name: string; kind: "compute" | "render"; ms: number }[]> {
  const each = [];
  for (const [i, p] of manifest.pipelines.entries()) {
    const started = performance.now();
    await buildPipeline(device, p, shaders[i]!);
    each.push({ name: p.name, kind: p.kind, ms: performance.now() - started });
  }
  return each;
}

/** The pipelines a build has, by a hash of each one's manifest entry and WGSL (`pipelineKey`):
 * what the next build keeps, in a hot reload. */
export type PipelineCache = Map<string, Promise<BuiltPipeline>>;

/** FNV-1a 64 of a pipeline's manifest entry and its WGSL, as hex. Not its WGSL file's name,
 * which is its place among the build's pipelines (`pipeline_3.wgsl`): a pipeline added or
 * dropped before it moves it, and the next build keeps it all the same. */
function pipelineKey(p: Pipeline, source: string): string {
  const hash = new StateHash();
  hash.update(new TextEncoder().encode(`${JSON.stringify({ ...p, shader: "" })}\n${source}`));
  return hash.hex();
}

/** Starts building every pipeline, all at once: each one's promise. `shaders[i]` is pipeline
 * `i`'s WGSL. With `cache` (a hot reload), a pipeline whose manifest entry and WGSL are the same
 * as one of the last build's is kept, and the cache then holds this build's pipelines alone. */
export function buildPipelines(
  device: GPUDevice,
  manifest: Manifest,
  shaders: string[],
  cache?: PipelineCache,
): Promise<BuiltPipeline>[] {
  // The biggest shaders start first: they take longest to compile, and the GPU process
  // compiles a few at a time, so started last they'd finish last of all.
  const order = manifest.pipelines.map((_, i) => i).sort((a, b) => shaders[b]!.length - shaders[a]!.length);
  const built: Promise<BuiltPipeline>[] = [];
  if (!cache) {
    for (const i of order) built[i] = buildPipeline(device, manifest.pipelines[i]!, shaders[i]!);
    return built;
  }
  const used: PipelineCache = new Map();
  for (const i of order) {
    const p = manifest.pipelines[i]!;
    const key = pipelineKey(p, shaders[i]!);
    const pipeline = used.get(key) ?? cache.get(key) ?? buildPipeline(device, p, shaders[i]!);
    used.set(key, pipeline);
    built[i] = pipeline;
  }
  cache.clear();
  for (const [key, pipeline] of used) cache.set(key, pipeline);
  return built;
}

/** A draw waiting for its pass to end, with its own copy of its uniform bytes. */
type PendingDraw = Extract<Command, { op: "Draw" | "DrawIndirect" | "DrawIndexedIndirect" }>;

/** A live resource the program made. */
type Resource =
  | { kind: "buffer"; buffer: GPUBuffer }
  | { kind: "texture"; texture: GPUTexture; view: GPUTextureView; format: TextureFormat; bytes: number }
  | { kind: "sampler"; sampler: GPUSampler };

/** Where a command was recorded: its frame, and which `frame` it came after (`Label`s name the
 * passes after them in that frame alone). */
type RecordedIn = { frame: number; generation: number };

/** A command kept for the serial mode's `drain`, or a readback waiting its turn. */
type Deferred =
  | { cmd: Command; at: RecordedIn }
  | { handle: number; offset: number; size: number; answer: PromiseWithResolvers<Bytes> };

/** `cmd` with its bytes copied out of the program's memory, which the program reuses. */
function copied(cmd: Command): Command {
  switch (cmd.op) {
    case "WriteBuffer":
    case "WriteTexture":
      return { ...cmd, data: cmd.data.slice() };
    case "Dispatch":
    case "DispatchIndirect":
    case "Draw":
    case "DrawIndirect":
    case "DrawIndexedIndirect":
      return { ...cmd, bindings: cmd.bindings.slice(), uniforms: cmd.uniforms.slice() };
    default:
      return cmd;
  }
}

/** `n` rounded up to a multiple of `align`. */
export const alignTo = (n: number, align: number) => Math.ceil(n / align) * align;

/** Whether passes and dispatches are timed, and how (runtime/native `Timing`): not at all; each
 * as it runs, beside the others; or each run alone, so its time is its own. */
export type Timing = "off" | "span" | "serial";

/** A pass's GPU time, as the native host records it (runtime/native `GpuTiming`). */
export interface GpuTiming {
  /** The frame it ran in (`GpuExecutor.frame`). */
  frame: number;
  /** The name the program gave it (`Label`), else the dispatch's pipeline name, or `screen pass`
   * or `pass`. */
  label: string;
  /** `end - start`. */
  nanos: number;
  /** When it started and ended on the GPU's clock, in nanoseconds from the first timestamp
   * read: a frame's span is its first start to its last end. */
  start: number;
  end: number;
  /** Which submission it ran in, counted from 0 over the run (the serial mode submits each pass
   * alone). */
  submission: number;
}

/** `capacity` doubled until it's at least `n`. */
function doubled(capacity: number, n: number): number {
  let c = capacity * 2;
  while (c < n) c *= 2;
  return c;
}

/** A GPU buffer and this submission's bytes for it, staged on the CPU and written to it just
 * before the submission (`write`): the uniform ring, and the upload ring (as the native host's
 * `Ring`). */
class Ring {
  buffer: GPUBuffer;
  bytes: Bytes;
  /** The bytes staged so far. */
  used = 0;

  constructor(
    readonly device: GPUDevice,
    readonly label: string,
    readonly usage: GPUBufferUsageFlags,
    capacity: number,
  ) {
    this.buffer = device.createBuffer({ label, size: capacity, usage });
    this.bytes = new Uint8Array(capacity);
  }

  /** Room for `n` bytes: a new buffer, its size `doubled` until they fit. Call it only after a
   * flush, which submitted the last work that used the old buffer. */
  grow(n: number): void {
    const capacity = doubled(this.bytes.length, n);
    this.buffer.destroy();
    this.buffer = this.device.createBuffer({ label: this.label, size: capacity, usage: this.usage });
    this.bytes = new Uint8Array(capacity);
  }

  /** Writes the staged bytes to the buffer, and starts again from 0. */
  write(queue: GPUQueue): void {
    if (this.used === 0) return;
    queue.writeBuffer(this.buffer, 0, this.bytes, 0, this.used);
    this.used = 0;
  }
}

/** Most passes timed in one submission: more flush first. */
const TIMED_PASSES = 256;

/** Timestamps at each pass's start and end ("timestamp-query"), read back after each
 * submission. */
class Timer {
  readonly #queries: GPUQuerySet;
  readonly #resolve: GPUBuffer;
  /** The passes timed in the submission being recorded: their frame and label. */
  #pending: { frame: number; label: string }[] = [];
  /** The first timestamp read, which the others are measured from. */
  #origin: bigint | null = null;
  /** The submissions timed so far. */
  #submissions = 0;
  /** The readbacks under way. */
  readonly #reading: Promise<void>[] = [];
  readonly results: GpuTiming[] = [];

  constructor(readonly device: GPUDevice) {
    this.#queries = device.createQuerySet({ label: "timestamps", type: "timestamp", count: 2 * TIMED_PASSES });
    this.#resolve = device.createBuffer({
      label: "timestamps",
      size: 16 * TIMED_PASSES,
      usage: GPUBufferUsage.QUERY_RESOLVE | GPUBufferUsage.COPY_SRC,
    });
  }

  get full(): boolean {
    return this.#pending.length === TIMED_PASSES;
  }

  /** The writes for the next pass. */
  next(frame: number, label: string): GPUComputePassTimestampWrites {
    const i = this.#pending.length;
    this.#pending.push({ frame, label });
    return { querySet: this.#queries, beginningOfPassWriteIndex: 2 * i, endOfPassWriteIndex: 2 * i + 1 };
  }

  /** Records the copy of this submission's timestamps, before its encoder finishes; `read`
   * reads them back after it's submitted. */
  resolve(encoder: GPUCommandEncoder): (() => void) | null {
    const passes = this.#pending.splice(0);
    if (passes.length === 0) return null;
    const submission = this.#submissions++;
    const bytes = 16 * passes.length;
    const staging = this.device.createBuffer({ label: "timestamps", size: bytes, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
    encoder.resolveQuerySet(this.#queries, 0, 2 * passes.length, this.#resolve, 0);
    encoder.copyBufferToBuffer(this.#resolve, 0, staging, 0, bytes);
    return () => {
      this.#reading.push(
        staging.mapAsync(GPUMapMode.READ).then(() => {
          const t = new BigUint64Array(staging.getMappedRange());
          passes.forEach((p, i) => {
            const [a, b] = [t[2 * i]!, t[2 * i + 1]!];
            this.#origin ??= a;
            const start = Number(a - this.#origin);
            // An end before its start (a clock that wrapped) is taken as the start.
            const end = Math.max(start, Number(b - this.#origin));
            this.results.push({ ...p, nanos: end - start, start, end, submission });
          });
          staging.unmap();
          staging.destroy();
        }),
      );
    };
  }

  /** Every timing read back so far, once the readbacks under way are done. */
  async settled(): Promise<GpuTiming[]> {
    await Promise.all(this.#reading.splice(0));
    return this.results;
  }
}

export class GpuExecutor {
  readonly #align: number;
  readonly #resources = new Map<number, Resource>();
  /** The bytes of the program's buffers and textures now, and at most so far. */
  #bytes = 0;
  #peak = 0;
  /** By pipeline and bindings. */
  readonly #bindGroups = new Map<string, { group: GPUBindGroup; handles: number[] }>();
  /** The keys of the bind groups that use each resource, so destroying it drops only those. */
  readonly #groupsOf = new Map<number, Set<string>>();
  /** Buffers and textures destroyed by the program, released once the work recorded before is
   * submitted. */
  readonly #doomed: (GPUBuffer | GPUTexture)[] = [];
  /** The uniform ring: this submission's uniform bytes. */
  readonly #uniforms: Ring;
  /** The upload ring: this submission's buffer writes, a copy of each from there recorded where
   * the write is, so a write splits no work into submissions (as the native host's upload
   * `Ring`). */
  readonly #uploads: Ring;
  #encoder: GPUCommandEncoder | null = null;
  #pass: OpenPass | null = null;
  /** A pass that ended, not yet recorded: the next pass continues it if it may join it, on the
   * same targets, keeping them (`joins`). Not in the serial mode, which times each alone. */
  #ended: OpenPass | null = null;
  readonly #timer: Timer | null;
  /** A debug build's: the flag its pipelines' bounds checks set (the manifest's `debug_flag`),
   * and the buffer it's read back through, made once. */
  readonly #debugFlag: { buffer: GPUBuffer; readback: GPUBuffer } | null;
  #frame = 0;
  /** How many times `frame` has been set: a command's label is its own frame's. */
  #generation = 0;
  /** Where the command running late (behind a pipeline being built, or in the serial mode's
   * `drain`) was recorded: its timings are that frame's, and so is its label. */
  #running: RecordedIn | null = null;
  /** Whether a pass on the screen has ended since this was last cleared: a frame that drew
   * nothing there leaves the screen as it was. */
  presented = false;
  /** The name the program gave the passes and dispatches after it in this frame (`Label`), and
   * the frame it was given in (its `generation`). */
  #label: string | null = null;
  #labelGeneration = 0;
  /** The frame being recorded, for timings. */
  get frame(): number {
    return this.#frame;
  }
  /** A frame begins: no label carries over from the last one to its commands (`#run`), even
   * while the last one's commands still run late. */
  set frame(i: number) {
    this.#frame = i;
    this.#generation++;
  }
  /** The serial timing mode's commands, waiting for `drain` to run them each pass and dispatch
   * alone; null in the other modes, which run each command as it comes. */
  readonly #deferred: Deferred[] | null;
  /** Each pipeline once it's built; `undefined` until then. The program starts while its
   * pipelines build: a command that needs one not built yet waits for it in `#waiting`, and
   * every command after it waits behind it, so they run in order. */
  readonly #pipelines: (BuiltPipeline | undefined)[];
  readonly #building: Promise<BuiltPipeline>[];
  readonly #waiting: Deferred[] = [];
  /** Resolved when no command waits any more (`caughtUp`). */
  #idle: PromiseWithResolvers<void> | null = null;
  /** Why a pipeline couldn't be built, or a command that waited for one failed when it ran:
   * the program has failed (`caughtUp` rejects with it, and `waiting` throws it). */
  #failed: unknown = null;
  /** Resolved when every pipeline is built; rejected, with why, if one can't be. */
  readonly built: Promise<void>;

  /** Timed, on a device with "timestamp-query", each pass's GPU time is recorded (`timings`); in
   * the serial mode each pass and dispatch runs alone, so its time is its own (the program's
   * commands then wait for `drain`, which test mode calls after each frame). */
  constructor(
    readonly device: GPUDevice,
    pipelines: (BuiltPipeline | Promise<BuiltPipeline>)[],
    readonly screen: ScreenTarget,
    timing: Timing = "off",
    debug = pipelines.some((p) => !(p instanceof Promise) && p.debugFlag !== null),
  ) {
    this.#deferred = timing === "serial" ? [] : null;
    this.#pipelines = pipelines.map((p) => (p instanceof Promise ? undefined : p));
    this.#building = pipelines.map((p) => Promise.resolve(p));
    this.built = Promise.all(this.#building).then(() => {});
    pipelines.forEach((p, i) => {
      if (!(p instanceof Promise)) return;
      p.then(
        (built) => {
          this.#pipelines[i] = built;
          // A command that waited and then fails fails the program, as it would have had it
          // run when it came: not lost in this callback, with the commands behind it waiting.
          try {
            this.#resume();
          } catch (e) {
            this.#fail(e);
          }
        },
        (e) => this.#fail(e),
      );
    });
    const l = device.limits;
    this.#align = Math.max(l.minUniformBufferOffsetAlignment, l.minStorageBufferOffsetAlignment);
    this.#uniforms = new Ring(
      device,
      "uniform ring",
      GPUBufferUsage.UNIFORM | GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
      RING_START,
    );
    this.#uploads = new Ring(device, "upload ring", GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST, UPLOAD_START);
    this.#timer = timing !== "off" && device.features.has("timestamp-query") ? new Timer(device) : null;
    this.#debugFlag = debug
      ? {
          buffer: device.createBuffer({ label: "debug flag", size: 4, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC }),
          readback: this.#readbackBuffer(4),
        }
      : null;
  }

  /** A debug build's: rejects if a pipeline has indexed an array out of range (its number, from
   * 1, is in the flag), once the GPU's work so far is done. One read at a time. As the native
   * host's `check_debug_flag`. */
  async checkDebugFlag(): Promise<void> {
    if (this.#debugFlag === null) return;
    this.flush();
    const bytes = await this.#copyBack(this.#debugFlag.buffer, 0, 4, this.#debugFlag.readback);
    const code = new DataView(bytes.buffer).getUint32(0, true);
    if (code === 0) return;
    const p = this.#pipelines[code - 1];
    const name = p === undefined ? `number ${code}` : `\`${p.name}\``;
    throw new Error(
      `GPU error: debug build: pipeline ${name} indexed an array out of range (in this frame, or a call before it); WGSL then reads or writes an element in range, or a zero`,
    );
  }

  /** Whether passes are timed. */
  get timed(): boolean {
    return this.#timer !== null;
  }

  /** Each timed pass's GPU time so far, once read back. */
  async timings(): Promise<GpuTiming[]> {
    this.flush();
    return this.#timer === null ? [] : this.#timer.settled();
  }

  /** Room for a pass's timestamps: flushes if the timer is full. Like every flush, it comes
   * before anything is recorded for the pass. */
  #reserveTimestamps(): void {
    if (this.#timer?.full) this.flush();
  }

  /** The timestamp writes for a pass about to be recorded; never flushes. */
  #timestamps(label: string): { timestampWrites?: GPUComputePassTimestampWrites } {
    return this.#timer === null ? {} : { timestampWrites: this.#timer.next(this.#running?.frame ?? this.frame, label) };
  }

  /** Carries out a command that has been decoded, sequenced and checked, or keeps a copy of it
   * for `drain` in the serial mode. Requests are the program's business, except a readback's
   * copy (`readBack`). */
  execute(cmd: Command): void {
    const at = { frame: this.#frame, generation: this.#generation };
    if (this.#deferred !== null) this.#deferred.push({ cmd: copied(cmd), at });
    else if (this.#waiting.length > 0 || !this.#ready(cmd)) this.#waiting.push({ cmd: copied(cmd), at });
    else this.#run(cmd);
  }

  /** Whether a command waits for a pipeline being built. Throws why the program failed, if a
   * pipeline couldn't be built or a command that waited for one failed. */
  get waiting(): boolean {
    if (this.#failed !== null) throw this.#failed;
    return this.#waiting.length > 0;
  }

  /** The program has failed, as `e` says (`#failed`). */
  #fail(e: unknown): void {
    this.#failed ??= e;
    this.#idle?.reject(e);
    this.#idle = null;
  }

  /** Resolves once no command waits for a pipeline being built (at once, if none does); rejects
   * if a pipeline couldn't be built. */
  caughtUp(): Promise<void> {
    if (this.#failed !== null) return Promise.reject(this.#failed);
    if (this.#waiting.length === 0) return Promise.resolve();
    this.#idle ??= Promise.withResolvers<void>();
    return this.#idle.promise;
  }

  /** Whether `cmd`'s pipeline, if it has one, is built. */
  #ready(cmd: Command): boolean {
    return !("pipeline" in cmd) || this.#pipelines[cmd.pipeline] !== undefined;
  }

  /** A pipeline was built: the commands waiting run, up to the next that needs one not built,
   * and what they recorded is submitted. */
  #resume(): void {
    if (this.#waiting.length === 0) return;
    while (this.#waiting.length > 0) {
      const d = this.#waiting[0]!;
      if ("cmd" in d && !this.#ready(d.cmd)) return;
      this.#waiting.shift();
      if ("cmd" in d) this.#runAs(d.at, d.cmd);
      else this.readBack(d.handle, d.offset, d.size, true).then(d.answer.resolve, d.answer.reject);
    }
    this.flush();
    this.#idle?.resolve();
    this.#idle = null;
  }

  /** Runs `cmd` as part of the frame it was recorded in (`at`): its timings and its label are
   * that frame's. */
  #runAs(at: RecordedIn, cmd: Command): void {
    this.#running = at;
    try {
      this.#run(cmd);
    } finally {
      this.#running = null;
    }
  }

  /** The serial mode: runs the commands kept since the last call, submitting each pass and
   * dispatch alone and waiting for the GPU to finish it before the next. */
  async drain(): Promise<void> {
    if (this.#deferred === null) return;
    for (const d of this.#deferred.splice(0)) {
      if ("cmd" in d && "pipeline" in d.cmd) await this.#building[d.cmd.pipeline];
      if ("cmd" in d) {
        // Its timings and its label are the frame's that recorded it, which may have been
        // before the one being recorded now.
        this.#running = d.at;
        try {
          this.#run(d.cmd);
        } catch (e) {
          throw new Error(`serial mode, ${d.cmd.op}: ${errorMessage(e)}`);
        } finally {
          this.#running = null;
        }
        const op = d.cmd.op;
        if (op === "Dispatch" || op === "DispatchIndirect" || op === "EndPass" || op === "Present") {
          this.flush();
          await this.device.queue.onSubmittedWorkDone();
        }
      } else {
        this.readBack(d.handle, d.offset, d.size, true).then(d.answer.resolve, d.answer.reject);
      }
    }
  }

  #run(cmd: Command): void {
    // A label names the passes and dispatches after it in its own frame: the first command of
    // another frame starts with none.
    const generation = this.#running?.generation ?? this.#generation;
    if (generation !== this.#labelGeneration) {
      this.#label = null;
      this.#labelGeneration = generation;
    }
    if (this.#ended !== null) {
      if (cmd.op === "BeginPass" && joins(cmd.pass, this.#ended.pass)) {
        this.#pass = this.#ended;
        this.#ended = null;
        return;
      }
      // A label names the passes after it, the next of which may yet continue this one.
      if (cmd.op !== "Label") this.#recordEnded();
    }
    switch (cmd.op) {
      case "CreateBuffer":
        this.#allocated(cmd.size);
        this.#resources.set(cmd.handle, {
          kind: "buffer",
          buffer: this.device.createBuffer({
            label: `buffer ${cmd.handle}`,
            size: cmd.size,
            usage:
              GPUBufferUsage.STORAGE |
              GPUBufferUsage.COPY_DST |
              GPUBufferUsage.COPY_SRC |
              GPUBufferUsage.INDIRECT |
              GPUBufferUsage.INDEX,
          }),
        });
        return;
      case "CreateTexture": {
        const depth = isDepth(cmd.format);
        // A 3D texture (`cmd.depth` texels deep) is sampled and written by kernels, never drawn
        // into.
        const three = cmd.depth > 0;
        const texture = this.device.createTexture({
          label: `texture ${cmd.handle}`,
          size: three ? [cmd.width, cmd.height, cmd.depth] : [cmd.width, cmd.height],
          dimension: three ? "3d" : "2d",
          format: cmd.format,
          usage:
            GPUTextureUsage.TEXTURE_BINDING |
            (three ? 0 : GPUTextureUsage.RENDER_ATTACHMENT) |
            GPUTextureUsage.COPY_SRC |
            (depth ? 0 : GPUTextureUsage.COPY_DST) |
            // Only where kernels write it: a storage texture may give up the GPU's compression.
            (cmd.writable ? GPUTextureUsage.STORAGE_BINDING : 0),
        });
        const bytes = cmd.width * cmd.height * Math.max(cmd.depth, 1) * bytesPerTexel(cmd.format);
        this.#resources.set(cmd.handle, { kind: "texture", texture, view: texture.createView(), format: cmd.format, bytes });
        this.#allocated(bytes);
        return;
      }
      case "CreateSampler": {
        const filter = cmd.linear ? "linear" : "nearest";
        const address = cmd.repeat ? "repeat" : "clamp-to-edge";
        const sampler = this.device.createSampler({
          label: `sampler ${cmd.handle}`,
          addressModeU: address,
          addressModeV: address,
          addressModeW: address,
          magFilter: filter,
          minFilter: filter,
          mipmapFilter: "nearest",
          ...(cmd.compare === null ? {} : { compare: cmd.compare }),
        });
        this.#resources.set(cmd.handle, { kind: "sampler", sampler });
        return;
      }
      case "WriteBuffer":
        this.#write(cmd.handle, cmd.offset, cmd.data);
        return;
      case "WriteTexture": {
        if (cmd.data.length === 0) return;
        this.flush();
        const t = this.#texture(cmd.handle);
        // Rows of `width` texels, no padding (the checker checked the length).
        this.device.queue.writeTexture(
          { texture: t.texture, origin: { x: cmd.x, y: cmd.y } },
          cmd.data,
          { bytesPerRow: cmd.data.length / cmd.height, rowsPerImage: cmd.height },
          { width: cmd.width, height: cmd.height },
        );
        return;
      }
      case "CopyBuffer":
        this.#encoderNow().copyBufferToBuffer(
          this.#buffer(cmd.source),
          cmd.sourceOffset,
          this.#buffer(cmd.destination),
          cmd.destinationOffset,
          cmd.size,
        );
        return;
      case "Dispatch":
        this.#dispatch(cmd.op, cmd.pipeline, cmd.groups, cmd.bindings, cmd.uniforms);
        return;
      case "DispatchIndirect":
        this.#dispatch(cmd.op, cmd.pipeline, { buffer: cmd.arguments, offset: cmd.offset }, cmd.bindings, cmd.uniforms);
        return;
      case "BeginScreenPass":
        this.#pass = {
          pass: { color: SCREEN, keepColor: false, join: false, clear: cmd.clear, depth: NONE, keepDepth: false, clearDepth: 1 },
          draws: [],
          label: this.#label,
        };
        return;
      case "BeginPass":
        this.#pass = { pass: cmd.pass, draws: [], label: this.#label };
        return;
      case "Draw":
      case "DrawIndirect":
      case "DrawIndexedIndirect":
        // The uniform bytes are a view into the program's memory: copy them.
        this.#pass?.draws.push({ ...cmd, uniforms: cmd.uniforms.slice() });
        return;
      case "Present":
        this.presented = true;
        this.#endPass(cmd.op);
        return;
      case "EndPass":
        this.#endPass(cmd.op);
        return;
      case "DestroyBuffer":
      case "DestroyTexture":
      case "DestroySampler":
        this.#destroy(cmd.handle);
        return;
      case "Label":
        this.#label = cmd.name;
        return;
      case "ReadBuffer":
      case "StorageRead":
      case "StorageWrite":
      case "Fetch":
      case "Log":
        return;
    }
  }

  /** Submits everything recorded so far, after writing its uniform bytes, then destroys the
   * resources the program destroyed (WebGPU waits for submitted work that uses them). */
  flush(): void {
    this.#recordEnded();
    if (this.#encoder !== null) {
      this.#uniforms.write(this.device.queue);
      this.#uploads.write(this.device.queue);
      const read = this.#timer?.resolve(this.#encoder) ?? null;
      this.device.queue.submit([this.#encoder.finish()]);
      this.#encoder = null;
      read?.();
    }
    for (const r of this.#doomed.splice(0)) r.destroy();
  }

  /** A buffer's bytes `offset..offset + size`, once the GPU's work recorded so far is done:
   * a `ReadBuffer` request's answer. In the serial mode it waits its turn behind the commands
   * kept for `drain`, unless `now`. */
  readBack(handle: number, offset: number, size: number, now = false): Promise<Bytes> {
    const queue = this.#deferred ?? (this.#waiting.length > 0 ? this.#waiting : null);
    if (queue !== null && !now) {
      const answer = Promise.withResolvers<Bytes>();
      queue.push({ handle, offset, size, answer });
      return answer.promise;
    }
    this.flush();
    const staging = this.#readbackBuffer(Math.max(size, 4));
    return this.#copyBack(this.#buffer(handle), offset, size, staging).then((bytes) => {
      staging.destroy();
      return bytes;
    });
  }

  /** Copies `source`'s bytes `offset..offset + size` to the CPU through `staging`, a readback
   * buffer at least `size` bytes long, once the GPU has done the copy. */
  async #copyBack(source: GPUBuffer, offset: number, size: number, staging: GPUBuffer): Promise<Bytes> {
    const encoder = this.device.createCommandEncoder();
    encoder.copyBufferToBuffer(source, offset, staging, 0, size);
    this.device.queue.submit([encoder.finish()]);
    await staging.mapAsync(GPUMapMode.READ);
    const bytes = new Uint8Array(staging.getMappedRange()).slice(0, size);
    staging.unmap();
    return bytes;
  }

  #readbackBuffer(size: number): GPUBuffer {
    return this.device.createBuffer({ label: "readback", size, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
  }

  /** The program's buffers and textures, in bytes: now, and at most so far (test mode's
   * `memory.json`). */
  get memory(): { bytes: number; peak: number } {
    return { bytes: this.#bytes, peak: this.#peak };
  }

  #allocated(bytes: number): void {
    this.#bytes += bytes;
    this.#peak = Math.max(this.#peak, this.#bytes);
  }

  /** Hot reload: every buffer and texture the program made, destroyed, after what's recorded
   * is submitted; the executor is used no more. */
  destroyAll(): void {
    this.#pass = null;
    for (const handle of [...this.#resources.keys()]) this.#destroy(handle);
    this.flush();
  }

  #destroy(handle: number): void {
    const r = this.#resources.get(handle);
    if (r === undefined) throw new Error(`host bug: resource ${handle} passed the checks but doesn't exist`);
    // Recorded work may still use it: it's destroyed once that work is submitted.
    if (r.kind === "buffer") {
      this.#doomed.push(r.buffer);
      this.#allocated(-r.buffer.size);
    }
    if (r.kind === "texture") {
      this.#doomed.push(r.texture);
      this.#allocated(-r.bytes);
    }
    this.#resources.delete(handle);
    for (const key of this.#groupsOf.get(handle) ?? []) this.#dropBindGroup(key);
    this.#groupsOf.delete(handle);
  }

  #buffer(handle: number): GPUBuffer {
    const r = this.#resources.get(handle);
    if (r?.kind !== "buffer") throw new Error(`host bug: buffer ${handle} passed the checks but doesn't exist`);
    return r.buffer;
  }

  #texture(handle: number): Extract<Resource, { kind: "texture" }> {
    const r = this.#resources.get(handle);
    if (r?.kind !== "texture") throw new Error(`host bug: texture ${handle} passed the checks but doesn't exist`);
    return r;
  }

  #pipeline(index: number): BuiltPipeline {
    const p = this.#pipelines[index];
    if (p === undefined) throw new Error(`host bug: pipeline ${index} passed the checks but isn't built`);
    return p;
  }

  /** A copy from the upload ring (`#uploads`): the stream's writes are whole words, as a copy
   * moves. A very big one goes through the queue after what's recorded is submitted. */
  #write(handle: number, offset: number, data: Bytes): void {
    const len = data.length;
    if (len === 0) return;
    if (len > UPLOAD_MAX) {
      // Work recorded before this write must see the old contents.
      this.flush();
      this.device.queue.writeBuffer(this.#buffer(handle), offset, data);
      return;
    }
    const up = this.#uploads;
    if (up.used + len > up.bytes.length) {
      this.flush();
      if (len > up.bytes.length) up.grow(len);
    }
    const at = up.used;
    // The bytes are a view into the program's memory: copied now.
    up.bytes.set(data, at);
    up.used += len;
    this.#encoderNow().copyBufferToBuffer(up.buffer, at, this.#buffer(handle), offset, len);
  }

  #encoderNow(): GPUCommandEncoder {
    this.#encoder ??= this.device.createCommandEncoder();
    return this.#encoder;
  }

  /** Makes room for `bytes` more uniform bytes (counted aligned) in this submission, flushing
   * first or growing the ring if it must. Call before recording the work that uses them. */
  #reserve(op: OpcodeName, bytes: number): void {
    const ring = this.#uniforms;
    if (alignTo(ring.used, this.#align) + bytes <= ring.bytes.length) return;
    this.flush();
    if (bytes > ring.bytes.length) {
      if (doubled(ring.bytes.length, bytes) > this.device.limits.maxBufferSize) {
        throw new CommandError(op, `${bytes} uniform bytes in one submission is over the limit`);
      }
      ring.grow(bytes);
      this.#bindGroups.clear(); // they point at the old ring
      this.#groupsOf.clear();
    }
  }

  /** Appends uniform bytes to this submission's slice of the ring; returns their offsets. */
  #push(bytes: Bytes): number[] {
    if (bytes.length === 0) return [];
    const ring = this.#uniforms;
    const offset = alignTo(ring.used, this.#align);
    ring.bytes.fill(0, ring.used, offset);
    ring.bytes.set(bytes, offset);
    ring.used = offset + bytes.length;
    return [offset];
  }

  #bindGroup(index: number, bindings: Binding[]): GPUBindGroup {
    // The pipeline, then each binding's range: built in one pass, as every dispatch and draw
    // makes one, cached or not.
    let key = `${index}:`;
    for (let i = 0; i < bindings.length; i++) {
      const b = bindings[i]!;
      if (i > 0) key += ",";
      key += b.handle + "@" + b.offset + "+" + b.size;
    }
    const cached = this.#bindGroups.get(key);
    if (cached !== undefined) return cached.group;
    const p = this.#pipeline(index);
    const entries: GPUBindGroupEntry[] = [];
    if (p.uniform !== null) {
      entries.push({ binding: p.uniform.binding, resource: { buffer: this.#uniforms.buffer, offset: 0, size: p.uniform.size } });
    }
    bindings.forEach((b, i) => {
      const r = this.#resources.get(b.handle);
      if (r === undefined) throw new Error(`host bug: resource ${b.handle} passed the checks but doesn't exist`);
      const resource: GPUBindingResource =
        r.kind === "buffer"
          ? { buffer: r.buffer, offset: b.offset, size: b.size }
          : r.kind === "texture"
            ? r.view
            : r.sampler;
      entries.push({ binding: p.bindings[i]!.binding, resource });
    });
    if (p.debugFlag !== null && this.#debugFlag !== null) {
      entries.push({ binding: p.debugFlag, resource: { buffer: this.#debugFlag.buffer } });
    }
    const group = this.device.createBindGroup({ label: p.name, layout: p.layout, entries });
    const handles = bindings.map((b) => b.handle);
    this.#bindGroups.set(key, { group, handles });
    for (const h of handles) {
      let keys = this.#groupsOf.get(h);
      if (keys === undefined) this.#groupsOf.set(h, (keys = new Set()));
      keys.add(key);
    }
    return group;
  }

  #dropBindGroup(key: string): void {
    const cached = this.#bindGroups.get(key);
    if (cached === undefined) return;
    this.#bindGroups.delete(key);
    for (const h of cached.handles) this.#groupsOf.get(h)?.delete(key);
  }

  #dispatch(
    op: OpcodeName,
    index: number,
    groups: [number, number, number] | { buffer: number; offset: number },
    bindings: Binding[],
    uniforms: Bytes,
  ): void {
    const p = this.#pipeline(index);
    if (p.compute === null) throw new Error(`host bug: pipeline ${index} isn't a compute pipeline`);
    // Everything that can flush happens before anything is recorded for this dispatch.
    this.#reserveTimestamps();
    this.#reserve(op, alignTo(uniforms.length, this.#align));
    const offsets = this.#push(uniforms);
    const bindGroup = this.#bindGroup(index, bindings);
    const label = this.#label ?? p.name;
    const timed = this.#timestamps(label);
    const pass = this.#encoderNow().beginComputePass({ label, ...timed });
    pass.setPipeline(p.compute);
    pass.setBindGroup(0, bindGroup, offsets);
    if (Array.isArray(groups)) pass.dispatchWorkgroups(groups[0], groups[1], groups[2]);
    else pass.dispatchWorkgroupsIndirect(this.#buffer(groups.buffer), groups.offset);
    pass.end();
  }

  /** The render pipeline of `index` for these targets: one of the manifest's for it, which the
   * checker checked, made at load. */
  #variant(index: number, color: GPUTextureFormat | null, depth: GPUTextureFormat | null): GPURenderPipeline {
    const p = this.#pipeline(index);
    const v = p.render?.variants.get(targetsKey(color, depth));
    if (v === undefined) throw new Error(`host bug: pipeline ${index} has no variant for its pass's targets`);
    return v;
  }

  /** Ends the open pass: on the screen (Present) it's recorded; into textures (EndPass), it
   * waits for the next command, which may continue it (`joins`), unless the mode is serial. */
  #endPass(op: OpcodeName): void {
    const open = this.#pass;
    this.#pass = null;
    if (open === null) throw new Error(`host bug: ${op} without a pass`);
    if (op === "EndPass" && this.#deferred === null) {
      this.#ended = open;
      return;
    }
    this.#recordPass(op, open);
  }

  /** Records the pass that ended and waits, if there is one. */
  #recordEnded(): void {
    const open = this.#ended;
    this.#ended = null;
    if (open !== null) this.#recordPass("EndPass", open);
  }

  /** Records a pass's draws into one render pass. */
  #recordPass(op: OpcodeName, open: OpenPass): void {
    const pass = open.pass;
    const onScreen = pass.color === SCREEN;
    const colorView = onScreen ? this.screen.texture().createView() : pass.color === NONE ? null : this.#texture(pass.color).view;
    const colorFormat: GPUTextureFormat | null = onScreen
      ? SCREEN_FORMAT
      : pass.color === NONE
        ? null
        : this.#texture(pass.color).format;
    const depth = pass.depth === NONE ? null : this.#texture(pass.depth);
    // Everything that can flush happens before anything is recorded for this pass.
    this.#reserveTimestamps();
    this.#reserve(op, open.draws.reduce((n, d) => n + alignTo(d.uniforms.length, this.#align), 0));
    // And everything that can throw: a pass begun is ended.
    const draws = open.draws.map((d) => ({
      draw: d,
      pipeline: this.#variant(d.pipeline, colorFormat, depth?.format ?? null),
      group: this.#bindGroup(d.pipeline, d.bindings),
      args: d.op === "Draw" ? null : this.#buffer(d.arguments),
      indices: d.op === "DrawIndexedIndirect" ? this.#buffer(d.indices) : null,
    }));
    const [r, g, b, a] = pass.clear;
    const label = open.label ?? (onScreen ? "screen pass" : "pass");
    const timed = this.#timestamps(label);
    const encoder = this.#encoderNow();
    const rp = encoder.beginRenderPass({
      label,
      ...timed,
      colorAttachments:
        colorView === null
          ? []
          : [
              {
                view: colorView,
                clearValue: { r, g, b, a },
                loadOp: pass.keepColor ? "load" : "clear",
                storeOp: "store",
              },
            ],
      ...(depth === null
        ? {}
        : {
            depthStencilAttachment: {
              view: depth.view,
              depthClearValue: pass.clearDepth,
              depthLoadOp: pass.keepDepth ? "load" : "clear",
              depthStoreOp: "store",
            },
          }),
    });
    for (const { draw: d, pipeline, group, args, indices } of draws) {
      const offsets = this.#push(d.uniforms);
      rp.setPipeline(pipeline);
      rp.setBindGroup(0, group, offsets);
      switch (d.op) {
        case "Draw":
          rp.draw(d.vertices, d.instances);
          break;
        case "DrawIndirect":
          rp.drawIndirect(args!, d.offset);
          break;
        case "DrawIndexedIndirect":
          rp.setIndexBuffer(indices!, "uint32", d.index_offset, d.index_size);
          rp.drawIndexedIndirect(args!, d.offset);
          break;
      }
    }
    rp.end();
    if (onScreen) {
      this.screen.afterPass?.(encoder);
      this.flush();
    }
  }
}

/** A pass between its beginning and `Present` or `EndPass`, and the name the program gave it. */
type OpenPass = { pass: Pass; draws: PendingDraw[]; label: string | null };
