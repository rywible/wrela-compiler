// The GPU side: builds the manifest's pipelines, and carries out decoded, sequenced and checked
// commands with WebGPU. The same mapping as the native host (runtime/native/src/gpu.rs):
//
// - Work is recorded into one pending command encoder and submitted ("flushed") at the end of a
//   pass on the screen, before an upload over UPLOAD_MAX bytes (so earlier work sees the old
//   contents), before a readback, and at the end of each call. Each dispatch
//   gets its own compute pass.
// - Other buffer and texture writes are staged in an upload ring, written just before the
//   flush, and a copy from there is recorded where the write is: it splits no work into
//   submissions (as the native host's upload `Ring`). Texture rows are padded to 256 bytes;
//   their padded size counts against UPLOAD_MAX.
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

import { BLENDS, NONE, SCREEN, SCREEN_FORMAT, UPLOAD_MAX, UPLOAD_START } from "./abi.gen.ts";
import { blockOf, bytesOf, CommandError, isDepth, levelSize, sampleType } from "./check.ts";
import { errorMessage } from "./errors.ts";
import { StateHash } from "./hash.ts";
import { type Blend, dimensionOf, formatOf, isDepthBinding, isStorage, type Manifest, type Pipeline, type ResourceBinding, type UniformBlock } from "./manifest.ts";
import {
  argumentsOf,
  type Binding,
  type Bytes,
  type Command,
  type DrawCommand,
  indicesOf,
  isDraw,
  joins,
  onePass,
  onScreen,
  type OpcodeName,
  type Pass,
  type TexelRect,
  type TextureFormat,
  type TextureKind,
} from "./stream.ts";

const RING_START = 64 * 1024;

const ENCODER = new TextEncoder();

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
  /** How each colour output meets its target's (the manifest's `blends`). */
  blends: Blend[];
  /** The manifest's `cull` and `depth_bias`. */
  cull: GPUCullMode;
  depthBias: { constant: number; slope_scale: number; clamp: number };
  /** The manifest's `depth`: how fragments' depths are tested, and whether they're kept. */
  depth: { compare: GPUCompareFunction; write: boolean };
  /** A legacy build's pipeline-overridable constants each stage is made with, by name. */
  constants: Record<string, number>;
  /** By target formats: `colours|depth`, the colours joined by commas, each a format or `none`. */
  variants: Map<string, GPURenderPipeline>;
}

export interface BuiltPipeline {
  name: string;
  layout: GPUBindGroupLayout;
  compute: GPUComputePipeline | null;
  render: RenderParts | null;
  /** The depth formats of its screen targets (null: none): what a variant for another screen
   * format takes (`GpuExecutor.screenVariants`). */
  screenDepths: (GPUTextureFormat | null)[];
  uniform: UniformBlock | null;
  bindings: ResourceBinding[];
  /** A debug build's: where its bounds checks' flag is bound. */
  debugFlag: number | null;
}

/** Whether `device` can do a capability of the format table: always, or with its feature. */
function can(device: GPUDevice, yes: boolean, needs: string | null): boolean {
  return yes && (needs === null || device.features.has(needs));
}

/** Where passes on the screen draw. */
export interface ScreenTarget {
  /** The format of the screen's textures, which the manifest's screen targets are built in: the
   * canvas's preferred format in a normal run (`bgra8unorm` on some devices, which hold the same
   * bytes as `SCREEN_FORMAT` in another order), `SCREEN_FORMAT` in test mode, which reads the
   * screen back. Every program of a worker draws the screen in one format, so the pipelines a
   * hot reload or a hand-over keeps (`PipelineCache`) are built in it. */
  format: GPUTextureFormat;
  /** The texture this frame's screen pass renders into, of `format`. */
  texture(): GPUTexture;
  /** Called after a screen pass is recorded, with its encoder, before it's submitted. */
  afterPass?(encoder: GPUCommandEncoder): void;
  /** Shows the screen another way (`ConfigureScreen`): its SDR format, and what makes the canvas
   * `format`, HDR or not, in a gamut (0 sRGB, 1 Display P3), whether it could. Without it (test
   * mode's screen) the command changes nothing. */
  show?: { sdr: GPUTextureFormat; apply(format: GPUTextureFormat, hdr: boolean, gamut: number): boolean };
  /** How it's shown, as `wrela.limit`'s `SCREEN_LIMIT` gives it (bit 0 HDR, bit 1 Display P3):
   * kept here, so a program a hot reload or a hand-over starts reads what the canvas shows. */
  state?: number;
}

/** The format the screen is drawn in. Test mode's is `SCREEN_FORMAT`, which it reads back and
 * copies to the canvas. A normal run draws straight into the canvas in the device's preferred
 * format (`bgra8unorm` on some), which spares the browser a copy of each frame. */
export function screenFormat(testing: boolean): GPUTextureFormat {
  return testing ? SCREEN_FORMAT : navigator.gpu.getPreferredCanvasFormat();
}

function compilationMessages(info: GPUCompilationInfo): string | null {
  const errors = info.messages.filter((m) => m.type === "error");
  if (errors.length === 0) return null;
  return errors.map((m) => `${m.lineNum}:${m.linePos}: ${m.message}`).join("\n");
}

/** The key of a render pipeline's variant for these target formats. */
const targetsKey = (colors: readonly GPUTextureFormat[], depth: GPUTextureFormat | null) => `${colors.length === 0 ? "none" : colors.join(",")}|${depth ?? "none"}`;

function renderDescriptor(
  name: string,
  parts: RenderParts,
  colors: readonly GPUTextureFormat[],
  depth: GPUTextureFormat | null,
): GPURenderPipelineDescriptor {
  return {
    label: name,
    layout: parts.layout,
    vertex: { module: parts.module, entryPoint: parts.vertex, buffers: [], constants: parts.constants },
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
      constants: parts.constants,
      targets: colors.map((format, c) => {
        const blend = BLENDS[parts.blends[c] ?? "replace"];
        return blend === null ? { format } : { format, blend: blend as GPUBlendState };
      }),
    },
  };
}


/** A colour texture binding's format, which the manifest's parser requires of it. */
function colourFormat(b: ResourceBinding): TextureFormat {
  if (b.format === null) throw new Error(`host bug: colour texture binding ${b.binding} has no format`);
  return b.format;
}

/** A shader's WGSL as this browser takes it: the compiler's (naga's, which wgpu's front end
 * parses without the directive) with `enable subgroups;` first when it uses them, which the WGSL
 * spec requires. `enable f16;` the compiler writes itself. */
export function shaderSource(p: Pipeline, source: string): string {
  return p.requires.includes("subgroups") ? `enable subgroups;\n${source}` : source;
}

/** Builds pipeline `p` from its WGSL; its screen targets' variants draw in `screenFormat`. */
async function buildPipeline(device: GPUDevice, p: Pipeline, source: string, screenFormat: GPUTextureFormat): Promise<BuiltPipeline> {
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
      case "texture_2d_array":
      case "texture_cube":
      case "depth_texture_2d_array":
        entries.push({
          binding: b.binding,
          visibility: seen,
          texture: {
            sampleType: isDepthBinding(b.kind) ? "depth" : sampleType(colourFormat(b)),
            viewDimension: dimensionOf(b.kind) as GPUTextureViewDimension,
          },
        });
        break;
      case "sampler":
      case "comparison_sampler":
      case "non_filtering_sampler":
        entries.push({
          binding: b.binding,
          visibility: seen,
          sampler: {
            type: b.kind === "sampler" ? "filtering" : b.kind === "comparison_sampler" ? "comparison" : "non-filtering",
          },
        });
        break;
      // Only a kernel writes a texture's texels.
      case "storage_texture":
      case "storage_texture_3d":
      case "storage_texture_2d_array":
        entries.push({
          binding: b.binding,
          visibility: GPUShaderStage.COMPUTE,
          storageTexture: {
            access: "write-only",
            format: colourFormat(b),
            viewDimension: dimensionOf(b.kind) as GPUTextureViewDimension,
          },
        });
        break;
    }
  }

  // A shader that needs a feature this device hasn't isn't built: the checker refuses a command
  // that records it, naming the feature.
  if (p.requires.some((f) => !device.features.has(f))) {
    const layout = device.createBindGroupLayout({ label: p.name, entries: [] });
    return { name: p.name, layout, compute: null, render: null, screenDepths: [], uniform: p.uniform, bindings: p.bindings, debugFlag: p.debug_flag };
  }
  // Pipelines build at the same time and share the device's error scope stack, so the scope is
  // popped before anything awaits. The async creations below report through their promises.
  device.pushErrorScope("validation");
  const layout = device.createBindGroupLayout({ label: p.name, entries });
  const pipelineLayout = device.createPipelineLayout({ label: p.name, bindGroupLayouts: [layout] });
  const module = device.createShaderModule({ label: p.shader, code: shaderSource(p, source) });
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
        compute: { module, entryPoint: p.entry, constants: Object.fromEntries(p.constants) },
      });
    } else {
      render = {
        module,
        layout: pipelineLayout,
        vertex: p.vertex_entry,
        fragment: p.fragment_entry,
        blends: p.blends,
        cull: p.cull,
        depthBias: p.depth_bias,
        depth: p.depth,
        constants: Object.fromEntries(p.constants),
        variants: new Map(),
      };
      // A variant for each of its targets (the passes the program draws it in, which the
      // compiler knows), side by side: a shader's errors come at load, and no frame waits to
      // compile one. The screen and a texture of its format share one.
      const targets = new Map<string, [GPUTextureFormat[], GPUTextureFormat | null]>();
      for (const t of p.targets) {
        // A target the device can't draw into (rg11b10ufloat without its feature) is one the
        // checker refuses a pass of.
        if (t.colors.some((c) => { const f = formatOf(c); return !can(device, f.renderable, f.needs.render); })) continue;
        // Nor one it can't blend into as the pipeline blends (a 32-bit float target without
        // float32-blendable): the program checks `features()` first, and the checker refuses a
        // draw that doesn't.
        if (t.colors.some((c, k) => {
          const f = formatOf(c);
          return (p.blends[k] ?? "replace") !== "replace" && !can(device, f.blendable, f.needs.blend);
        })) continue;
        const colors = t.screen ? [screenFormat] : (t.colors as GPUTextureFormat[]);
        const depth = t.depth ? "depth32float" : null;
        targets.set(targetsKey(colors, depth), [colors, depth]);
      }
      const made = await Promise.all(
        [...targets.values()].map(([c, d]) => device.createRenderPipelineAsync(renderDescriptor(p.name, render!, c, d))),
      );
      [...targets.keys()].forEach((key, i) => render!.variants.set(key, made[i]!));
    }
  } catch (e) {
    throw fail(errorMessage(e));
  }
  const screenDepths =
    p.kind === "compute" ? [] : p.targets.filter((t) => t.screen).map((t): GPUTextureFormat | null => (t.depth ? "depth32float" : null));
  return { name: p.name, layout, compute, render, screenDepths, uniform: p.uniform, bindings: p.bindings, debugFlag: p.debug_flag };
}

/** How long building each pipeline of `manifest` alone took (ms), one after another: where a
 * build's time to create its pipelines goes (test mode's `salt`). */
export async function timeEachPipeline(
  device: GPUDevice,
  manifest: Manifest,
  shaders: string[],
  screenFormat: GPUTextureFormat,
): Promise<{ name: string; kind: "compute" | "render"; ms: number }[]> {
  const each = [];
  for (const [i, p] of manifest.pipelines.entries()) {
    const started = performance.now();
    await buildPipeline(device, p, shaders[i]!, screenFormat);
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
  hash.update(ENCODER.encode(`${JSON.stringify({ ...p, shader: "" })}\n${source}`));
  return hash.hex();
}

/** The pipelines' indices, the biggest shader first: those take longest to compile, and the GPU
 * process compiles a few at a time, so started last they'd finish last of all. */
function biggestFirst(shaders: string[]): number[] {
  return shaders.map((_, i) => i).sort((a, b) => shaders[b]!.length - shaders[a]!.length);
}

// Background preparation shares the GPU process with the visible program, even across
// embedded pages. Submitting every shader at once can block that process for hundreds of
// milliseconds. Keep one compilation in flight per device and leave a frame-sized gap for
// drawing between shaders. Foreground startup still builds its pipelines together.
const preparations = new WeakMap<GPUDevice, Promise<void>>();

function preparePipeline(device: GPUDevice, pipeline: Pipeline, shader: string, screenFormat: GPUTextureFormat): Promise<BuiltPipeline> {
  const previous = preparations.get(device) ?? Promise.resolve();
  const built = previous
    .then(() => new Promise<void>((resolve) => setTimeout(resolve, 16)))
    .then(() => buildPipeline(device, pipeline, shader, screenFormat));
  // A bad shader fails its caller, but must not poison the next preparation on this device.
  const settled = built.then(() => {}, () => {});
  preparations.set(device, settled);
  void settled.then(() => { if (preparations.get(device) === settled) preparations.delete(device); });
  return built;
}

/** Queues the pipelines of a build got ready for a hand-over (`std::reload::prepare`)
 * that none of `have` holds: the build's pipelines by key, including pending preparations. Its
 * screen targets draw in `screenFormat` (`ScreenTarget.format`). */
export function preparePipelines(
  device: GPUDevice,
  manifest: Manifest,
  shaders: string[],
  screenFormat: GPUTextureFormat,
  have: readonly PipelineCache[],
): PipelineCache {
  const out: PipelineCache = new Map();
  for (const i of biggestFirst(shaders)) {
    const p = manifest.pipelines[i]!;
    const key = pipelineKey(p, shaders[i]!);
    if (out.has(key)) continue;
    const held = have.map((c) => c.get(key)).find((b) => b !== undefined);
    out.set(key, held ?? preparePipeline(device, p, shaders[i]!, screenFormat));
  }
  return out;
}

/** A build's pipelines as they're built: each one's promise, in manifest order; `tier0`, settled
 * when every tier-0 pipeline is (`Pipeline.conditional` is tier 1); `start(i)`, which starts
 * pipeline `i`'s build now if it hasn't started (a command needs it before its tier's turn);
 * and the order the builds started in. */
export interface Building {
  each: Promise<BuiltPipeline>[];
  tier0: Promise<unknown>;
  start(i: number): void;
  order: number[];
}

/** How many pipelines build at once, besides those a command waits for (which start at once):
 * the GPU process compiles a few at a time however many are asked for, and asked for all at
 * once, one a frame needs waits for the rest. Six on the M4 Air (rywible/wrela's C5, three
 * starts each): the floor's warm start 6.5 s with all at once, 5.4 s with six; the vale's cold
 * one 8.0 s, 6.7 s (two and twelve were each worse at one of the two). */
export const BUILDS_AT_ONCE = 6;

/** Starts building the pipelines, `BUILDS_AT_ONCE` at a time: tier 0's, the biggest first, then
 * tier 1's (those reached only under a runtime condition); one a command needs before its turn
 * (`Building.start`) starts then, beside them. `shaders[i]` is pipeline `i`'s WGSL, and screen targets
 * draw in `screenFormat` (`ScreenTarget.format`). With `cache` (a hot reload), a pipeline whose
 * manifest entry and WGSL are the same as one of the last build's is kept, and the cache then
 * holds this build's pipelines alone. */
export function buildPipelines(
  device: GPUDevice,
  manifest: Manifest,
  shaders: string[],
  screenFormat: GPUTextureFormat,
  cache?: PipelineCache,
): Building {
  const order = biggestFirst(shaders);
  const each: Promise<BuiltPipeline>[] = [];
  const started: number[] = [];
  // The pipelines not started yet, in the order they start: tier 0's, then tier 1's.
  const queued: number[] = [];
  const resolvers = new Map<number, PromiseWithResolvers<BuiltPipeline>>();
  let running = 0;
  // Builds pipeline `i`, counted against `BUILDS_AT_ONCE` unless a command asked for it.
  const launch = (i: number, counted: boolean) => {
    const d = resolvers.get(i)!;
    resolvers.delete(i);
    if (counted) running++;
    started.push(i);
    const done = () => {
      if (counted) running--;
      pump();
    };
    buildPipeline(device, manifest.pipelines[i]!, shaders[i]!, screenFormat).then(
      (b) => {
        d.resolve(b);
        done();
      },
      (e: unknown) => {
        d.reject(e);
        done();
      },
    );
  };
  const pump = () => {
    while (running < BUILDS_AT_ONCE && queued.length > 0) {
      const i = queued.shift()!;
      if (resolvers.has(i)) launch(i, true);
    }
  };
  const queue = (i: number): Promise<BuiltPipeline> => {
    const d = Promise.withResolvers<BuiltPipeline>();
    resolvers.set(i, d);
    return d.promise;
  };
  const used: PipelineCache = new Map();
  for (const i of order) {
    const p = manifest.pipelines[i]!;
    const key = cache ? pipelineKey(p, shaders[i]!) : "";
    const kept = cache ? (used.get(key) ?? cache.get(key)) : undefined;
    const pipeline = kept ?? queue(i);
    if (cache) used.set(key, pipeline);
    each[i] = pipeline;
  }
  for (const tier of [false, true]) {
    for (const i of order) if (resolvers.has(i) && manifest.pipelines[i]!.conditional === tier) queued.push(i);
  }
  if (cache) {
    cache.clear();
    for (const [key, pipeline] of used) cache.set(key, pipeline);
  }
  const tier0 = Promise.all(order.filter((i) => !manifest.pipelines[i]!.conditional).map((i) => each[i]!));
  // A failure fails the program through its own pipeline's promise, not this one.
  tier0.catch(() => {});
  pump();
  const start = (i: number) => {
    if (resolvers.has(i)) launch(i, false);
  };
  return { each, tier0, start, order: started };
}

/** What a pass records, waiting for it to end: draws, with their own copies of their uniform
 * bytes, and the scissors and viewports set between them. */
type PendingDraw = DrawCommand | Extract<Command, { op: "SetScissor" | "SetViewport" }>;

/** A live texture: its views, by what each holds and its dimension, made once each. */
type TextureResource = {
  kind: "texture";
  texture: GPUTexture;
  format: TextureFormat;
  textureKind: TextureKind;
  levels: number;
  bytes: number;
  views: Map<string, GPUTextureView>;
};

/** A live resource the program made. */
type Resource = { kind: "buffer"; buffer: GPUBuffer } | TextureResource | { kind: "sampler"; sampler: GPUSampler };

/** A view of `t`'s levels and layers in `dimension`: `level` and `layer` one each, or null for
 * all (`storage`: one level, the first if it names none, as a storage binding takes). */
function viewOf(t: TextureResource, level: number | null, layer: number | null, dimension: GPUTextureViewDimension, storage: boolean): GPUTextureView {
  const key = `${level}:${layer}:${dimension}:${storage}`;
  let v = t.views.get(key);
  if (v === undefined) {
    const lv = level ?? (storage ? 0 : null);
    v = t.texture.createView({
      dimension,
      baseMipLevel: lv ?? 0,
      ...(lv === null ? {} : { mipLevelCount: 1 }),
      baseArrayLayer: layer ?? 0,
      ...(layer === null ? {} : { arrayLayerCount: 1 }),
    });
    t.views.set(key, v);
  }
  return v;
}

/** A cached group owns its binding ranges: commands may reuse or change their bindings. */
type CachedBindGroup = { group: GPUBindGroup; pipeline: number; handles: number[]; ranges: number[] };

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
    case "DrawIndexed":
    case "DrawIndexedIndirect":
    case "MultiDrawIndirect":
    case "MultiDrawIndexedIndirect":
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

/** Each of `frames` frames' labels and their GPU ms, largest first: `[label, ms]`s. */
export function frameLabels(timings: GpuTiming[], frames: number): [string, number][][] {
  const per: Map<string, number>[] = Array.from({ length: frames }, () => new Map());
  for (const t of timings) {
    const m = per[t.frame];
    if (m !== undefined) m.set(t.label, (m.get(t.label) ?? 0) + t.nanos / 1e6);
  }
  return per.map((m) => [...m.entries()].sort((a, b) => b[1] - a[1]));
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

  /** Writes the staged bytes to the buffer, counted in `counters`, and starts again from 0. */
  write(queue: GPUQueue, counters: Counters): void {
    if (this.used === 0) return;
    queue.writeBuffer(this.buffer, 0, this.bytes, 0, this.used);
    counters.write_buffers++;
    counters.write_buffer_bytes += this.used;
    this.used = 0;
  }
}

/** What an executor did below the recorded stream, since its counters were last taken: the
 * physical half of a frame's draw count (G-draws). A multi-draw the device runs in one call is one
 * `draw_calls` and its `count` `draws`; without the feature each draw is a call. */
export interface Counters {
  compute_passes: number;
  render_passes: number;
  dispatches: number;
  draws: number;
  draw_calls: number;
  write_buffers: number;
  write_buffer_bytes: number;
  write_textures: number;
  bind_groups: number;
  pipelines: number;
}

export const noCounters = (): Counters => ({
  compute_passes: 0,
  render_passes: 0,
  dispatches: 0,
  draws: 0,
  draw_calls: 0,
  write_buffers: 0,
  write_buffer_bytes: 0,
  write_textures: 0,
  bind_groups: 0,
  pipelines: 0,
});

/** A copy's extent in whole blocks of `format`: a rectangle of a compressed texture that reaches
 * its level's edge (the checker allows no other part block) covers that edge's last block, which
 * WebGPU counts in the level's physical size (a 4 × 4 BC level 1 is one 4 × 4 block). */
function wholeBlocks(format: TextureFormat, width: number, height: number): GPUExtent3DDict {
  const b = blockOf(format);
  return { width: alignTo(width, b), height: alignTo(height, b) };
}

/** A compute pass a run of dispatches shares, and what it last set. */
type OpenCompute = { pass: GPUComputePassEncoder; label: string; pipeline: GPUComputePipeline | null; group: GPUBindGroup | null; offsets: number[] };

/** Whether two dynamic offset lists are the same. */
function sameOffsets(a: readonly number[], b: readonly number[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** Most passes timed in one submission: more flush first. */
const TIMED_PASSES = 256;

/** What `#timestamps` gives an untimed pass: no timestamp writes. */
const UNTIMED: { timestampWrites?: GPUComputePassTimestampWrites } = Object.freeze({});

/** A program's ask for a label's time (`std::gpu::timed`). */
interface Ask {
  label: string;
  /** Where its `Timed` command ran among the passes and dispatches timed (`Timer.noted` then),
   * or null until it has run: it times only its label's work after that, in the stream's order. */
  from: number | null;
  /** Its answer, once some of that work is read back: the first frame that ran some, and its
   * nanoseconds there, summed. */
  time: { frame: number; nanos: number } | null;
  answer: (bytes: Bytes) => void;
}

/** Timestamps at each pass's start and end ("timestamp-query"), read back after each
 * submission. */
class Timer {
  readonly #queries: GPUQuerySet;
  readonly #resolve: GPUBuffer;
  /** The passes timed in the submission being recorded: their frame, label and place (`noted`). */
  #pending: { frame: number; label: string; at: number }[] = [];
  /** The first timestamp read, which the others are measured from. */
  #origin: bigint | null = null;
  #noted = 0;
  /** Called after each readback: what waits for a label's time looks again. */
  onRead: (() => void) | null = null;
  /** The submissions timed so far. */
  #submissions = 0;
  /** The readbacks under way, each until it's done. */
  readonly #reading = new Set<Promise<void>>();
  /** How many readbacks under way hold each frame's passes. */
  readonly #outstanding = new Map<number, number>();
  /** Why a readback failed, if one did, for `settled`: it's no longer held once done. */
  #failure: unknown = null;
  readonly results: GpuTiming[] = [];

  /** `keep`: every timing is kept (`settled`), for a run timed from its start; a timer the
   * program started (`std::gpu::timed`) keeps only what answers its `asks`. */
  constructor(
    readonly device: GPUDevice,
    readonly asks: readonly Ask[],
    readonly keep = true,
  ) {
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

  /** The passes and dispatches timed so far: where an ask starts (`Ask.from`). */
  get noted(): number {
    return this.#noted;
  }

  /** The writes for the next pass. */
  next(frame: number, label: string): GPUComputePassTimestampWrites {
    const i = this.#pending.length;
    this.#pending.push({ frame, label, at: this.#noted++ });
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
    const frames = [...new Set(passes.map((p) => p.frame))];
    for (const f of frames) this.#outstanding.set(f, (this.#outstanding.get(f) ?? 0) + 1);
    return () => {
      const reading = staging.mapAsync(GPUMapMode.READ).then(() => {
        const t = new BigUint64Array(staging.getMappedRange());
        passes.forEach((p, i) => {
          const [a, b] = [t[2 * i]!, t[2 * i + 1]!];
          this.#origin ??= a;
          const start = Number(a - this.#origin);
          // An end before its start (a clock that wrapped) is taken as the start.
          const end = Math.max(start, Number(b - this.#origin));
          const nanos = end - start;
          // It counts for each ask of its label made before it, in the first frame with some of
          // that ask's work (an earlier frame's, read back later, would replace a later one's).
          for (const ask of this.asks) {
            if (ask.label !== p.label || ask.from === null || ask.from > p.at) continue;
            if (ask.time === null || p.frame < ask.time.frame) ask.time = { frame: p.frame, nanos };
            else if (p.frame === ask.time.frame) ask.time.nanos += nanos;
          }
          if (this.keep) this.results.push({ frame: p.frame, label: p.label, nanos, start, end, submission });
        });
        staging.unmap();
      }).finally(() => {
        staging.destroy();
        this.#reading.delete(reading);
        for (const f of frames) {
          const n = this.#outstanding.get(f)! - 1;
          if (n === 0) this.#outstanding.delete(f);
          else this.#outstanding.set(f, n);
        }
        this.onRead?.();
      });
      // A failed map may arrive before timings() waits for it: kept for settled().
      void reading.catch((e: unknown) => {
        this.#failure ??= e;
      });
      this.#reading.add(reading);
    };
  }

  /** All uses have been submitted; their readbacks own separate staging buffers. */
  destroy(): void {
    this.#queries.destroy();
    this.#resolve.destroy();
  }

  /** Every timing read back so far, once the readbacks under way are done. */
  async settled(): Promise<GpuTiming[]> {
    await Promise.all([...this.#reading]);
    if (this.#failure !== null) throw this.#failure;
    return this.results;
  }

  /** Whether every readback that holds frame `frame`'s passes, or an earlier frame's, is done. */
  complete(frame: number): boolean {
    for (const f of this.#outstanding.keys()) if (f <= frame) return false;
    return true;
  }
}

export class GpuExecutor {
  readonly #align: number;
  /** Whether the device draws a multi-draw in one call ("chromium-experimental-multi-draw-indirect"). */
  readonly #multiDraw: boolean;
  readonly #resources = new Map<number, Resource>();
  #counters: Counters = noCounters();
  /** The bytes of the program's buffers and textures now, and at most so far. */
  #bytes = 0;
  #peak = 0;
  /** By pipeline and bindings. */
  readonly #bindGroups = new Map<string, CachedBindGroup>();
  /** Each pipeline's last group avoids building a string key for unchanged binding ranges. */
  readonly #lastGroups: (CachedBindGroup | undefined)[] = [];
  /** The keys of the bind groups that use each resource, so destroying it drops only those. */
  readonly #groupsOf = new Map<number, Set<string>>();
  /** Buffers and textures destroyed by the program, released once the work recorded before is
   * submitted. */
  readonly #doomed: (GPUBuffer | GPUTexture)[] = [];
  /** The uniform ring: this submission's uniform bytes. */
  readonly #uniforms: Ring;
  /** The upload ring: this submission's buffer and texture writes, a copy of each recorded where
   * the write is, so a write splits no work into submissions (as the native host's upload
   * `Ring`). */
  readonly #uploads: Ring;
  #encoder: GPUCommandEncoder | null = null;
  /** The compute pass a run of dispatches shares (untimed: a timed dispatch is its own pass,
   * for its timestamps), and what it last set; ended by anything else the encoder records. */
  #compute: OpenCompute | null = null;
  #pass: OpenPass | null = null;
  /** A pass that ended, not yet recorded: the next pass in its frame continues it if it may join
   * it, on the same targets, keeping them (`joins`), under the same label (a `Label` between
   * them records this one). Not in the serial mode, which times each alone. */
  #ended: OpenPass | null = null;
  #timer: Timer | null;
  /** The program's asks for a label's time (`std::gpu::timed`) not yet answered. Each starts
   * where its `Timed` command runs, which work behind a pipeline being built does late. */
  readonly #asked: Ask[] = [];
  /** `Timed` commands that ran before their asks arrived (commands run, then their requests
   * start), by label: where each ran (`Timer.noted`), oldest first. */
  readonly #early = new Map<string, number[]>();
  /** A debug build's: the flag its pipelines' bounds checks set (the manifest's `debug_flag`),
   * and the buffer it's read back through, made once. */
  readonly #debugFlag: { buffer: GPUBuffer; readback: GPUBuffer } | null;
  #debugReading = false;
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
    if (this.#asked.length > 0) this.#answerTimes();
  }
  /** The serial timing mode's commands, waiting for `drain` to run them each pass and dispatch
   * alone; null in the other modes, which run each command as it comes. */
  readonly #deferred: Deferred[] | null;
  /** Commands a serial drain has taken, including readbacks still waiting their turn. */
  readonly #draining = new Set<Deferred[]>();
  /** Each pipeline once it's built; `undefined` until then. The program starts while its
   * pipelines build: a command that needs one not built yet waits for it in `#waiting`, and
   * every command after it waits behind it, so they run in order. */
  readonly #pipelines: (BuiltPipeline | undefined)[];
  readonly #building: Promise<BuiltPipeline>[];
  readonly #waiting: (Deferred | undefined)[] = [];
  /** Consumed slots are cleared in place: a backlog drains without shifting every command. */
  #waitingAt = 0;
  /** Resolved when no command waits any more (`caughtUp`). */
  #idle: PromiseWithResolvers<void> | null = null;
  /** Why a pipeline couldn't be built, or a command that waited for one failed when it ran:
   * the program has failed (`caughtUp` rejects with it, and `waiting` throws it). */
  #failed: unknown = null;
  /** Hot reload retired this executor: no asynchronous pipeline completion may run more work. */
  #retired = false;
  /** Resolved when every tier-0 pipeline is built (every pipeline, unless the executor was
   * given its tiers); rejected, with why, if one can't be. */
  readonly built: Promise<void>;
  /** Starts building pipeline `i` now, if it hasn't started (`Building.start`). */
  readonly #start: (i: number) => void;
  /** When each pipeline was built (`performance.now()`), or null if it was built before the
   * program started or isn't yet; and each time a command waited for one (its frame, the
   * pipeline, and when): test mode's `starts.json`, what a start waited for. */
  readonly builtAt: (number | null)[];
  readonly waits: { frame: number; pipeline: number; at: number }[] = [];
  /** How the screen is shown, as `wrela.limit`'s `SCREEN_LIMIT` gives it: bit 0 HDR, bit 1
   * Display P3 (`ScreenTarget.state`). It changes once a `ConfigureScreen` is done. */
  get screenState(): number {
    return this.screen.state ?? 0;
  }
  /** The screen the latest `ConfigureScreen` asked for, while its variants are built; only
   * that request is applied when it's ready, never one it replaced. */
  #screenWanted: number | undefined;
  #screenAsked = 0;

  /** A variant of each render pipeline that draws the screen, in `format`: what showing the
   * screen in another format needs first. Every pipeline is started, so none is built later in
   * the old format alone. */
  async screenVariants(format: GPUTextureFormat): Promise<void> {
    this.#building.forEach((_, i) => this.#start(i));
    const built = await Promise.all(this.#building);
    await Promise.all(
      built.flatMap((b) => {
        const render = b.render;
        if (render === null) return [];
        return b.screenDepths.map(async (depth) => {
          const key = targetsKey([format], depth);
          if (render.variants.has(key)) return;
          const made = await this.device.createRenderPipelineAsync(renderDescriptor(b.name, render, [format], depth));
          render.variants.set(key, made);
        });
      }),
    );
  }

  /** Timed, on a device with "timestamp-query", each pass's GPU time is recorded (`timings`); in
   * the serial mode each pass and dispatch runs alone, so its time is its own (the program's
   * commands then wait for `drain`, which test mode calls after each frame). */
  constructor(
    readonly device: GPUDevice,
    pipelines: (BuiltPipeline | Promise<BuiltPipeline>)[],
    readonly screen: ScreenTarget,
    timing: Timing = "off",
    debug = pipelines.some((p) => !(p instanceof Promise) && p.debugFlag !== null),
    tiers?: Pick<Building, "tier0" | "start">,
  ) {
    this.#deferred = timing === "serial" ? [] : null;
    this.#pipelines = pipelines.map((p) => (p instanceof Promise ? undefined : p));
    this.builtAt = pipelines.map(() => null);
    this.#building = pipelines.map((p) => Promise.resolve(p));
    this.#start = tiers?.start ?? (() => {});
    this.built = (tiers ? tiers.tier0 : Promise.all(this.#building)).then(
      () => {},
      (e: unknown) => {
        // A superseded build's late shader failure belongs to it, not to the running build.
        if (!this.#retired) throw e;
      },
    );
    pipelines.forEach((p, i) => {
      if (!(p instanceof Promise)) return;
      p.then(
        (built) => {
          if (this.#retired || this.#failed !== null) return;
          this.#pipelines[i] = built;
          this.builtAt[i] = performance.now();
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
    this.#multiDraw = device.features.has("chromium-experimental-multi-draw-indirect");
    this.#uniforms = new Ring(
      device,
      "uniform ring",
      GPUBufferUsage.UNIFORM | GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
      RING_START,
    );
    this.#uploads = new Ring(device, "upload ring", GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST, UPLOAD_START);
    this.#timer = timing !== "off" && device.features.has("timestamp-query") ? new Timer(device, this.#asked) : null;
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
    if (this.#debugFlag === null || this.#retired) return;
    this.#debugReading = true;
    let bytes: Bytes;
    try {
      this.flush();
      bytes = await this.#copyBack(this.#debugFlag.buffer, 0, 4, this.#debugFlag.readback);
    } finally {
      this.#debugReading = false;
      // Retiring must leave a mapping in flight alive until its reader has finished.
      if (this.#retired) this.#debugFlag.readback.destroy();
    }
    if (this.#retired) return;
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

  /** The GPU time of the passes and dispatches labelled `label` that the stream records after
   * the ask's `Timed` command, in the first frame that runs one, summed: an `f32` of
   * microseconds, once read back. Work before the command doesn't count, even in its frame, as
   * on the native host. The first ask starts a timer, on a device with "timestamp-query";
   * without it, this rejects. Outside a timed run, only the work of a label asked for and not
   * yet answered is timed (also while commands wait for a pipeline): the rest runs as untimed,
   * its dispatches sharing a pass. */
  timeLabel(label: string): Promise<Bytes> {
    // The ask uses up its `Timed` command, if that ran first, even when it's refused.
    const early = this.#early.get(label);
    const from = early?.shift() ?? null;
    if (early?.length === 0) this.#early.delete(label);
    if (this.#timer === null) {
      if (!this.device.features.has("timestamp-query")) {
        return Promise.reject(new Error("this GPU can't time its work: it has no timestamp queries"));
      }
      this.#timer = new Timer(this.device, this.#asked, false);
    }
    this.#timer.onRead ??= () => this.#answerTimes();
    return new Promise((answer) => this.#asked.push({ label, from, time: null, answer }));
  }

  /** Whether a command recorded in frame `frame` or earlier still waits to run (behind a
   * pipeline being built, or for the serial mode's `drain`): its passes aren't timed yet. */
  #holds(frame: number): boolean {
    const waits = (q: (Deferred | undefined)[], from: number) => {
      for (let i = from; i < q.length; i++) {
        const d = q[i];
        if (d !== undefined && "cmd" in d && d.at.frame <= frame) return true;
      }
      return false;
    };
    if (waits(this.#waiting, this.#waitingAt) || (this.#deferred !== null && waits(this.#deferred, 0))) return true;
    // A batch the serial drain is running holds its frames until it's done.
    for (const batch of this.#draining) if (waits(batch, 0)) return true;
    return false;
  }

  /** Answers each ask some of whose work has been read back (`Ask.time`), once that work's frame
   * has ended, all its timestamps are read and none of its commands waits: the frame's whole
   * time, not a submission's. */
  #answerTimes(): void {
    const timer = this.#timer;
    if (timer === null) return;
    for (let i = this.#asked.length - 1; i >= 0; i--) {
      const ask = this.#asked[i]!;
      const t = ask.time;
      if (t === null || t.frame >= this.#frame || !timer.complete(t.frame) || this.#holds(t.frame)) continue;
      this.#asked.splice(i, 1);
      ask.answer(new Uint8Array(new Float32Array([t.nanos / 1000]).buffer));
    }
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

  /** The timestamp writes for a pass about to be recorded, if it's timed: in a timed run every
   * pass, else only one whose label the program has asked for and not yet had; never flushes. */
  #timestamps(label: string, frame = this.#running?.frame ?? this.frame): { timestampWrites?: GPUComputePassTimestampWrites } {
    const timer = this.#timer;
    if (timer === null || !(timer.keep || this.#asked.some((a) => a.from !== null && a.label === label))) return UNTIMED;
    return { timestampWrites: timer.next(frame, label) };
  }

  /** Carries out a command that has been decoded, sequenced and checked, or keeps a copy of it
   * for `drain` in the serial mode. Requests are the program's business, except a readback's
   * copy (`readBack`). A pipeline past the manifest's fails at once, unchecked too: a command
   * that waited for it would wait forever. */
  execute(cmd: Command): void {
    if (this.#retired) throw new Error("host bug: a retired GPU executor received a command");
    if ("pipeline" in cmd && cmd.pipeline >= this.#pipelines.length) {
      throw new CommandError(cmd.op, `there's no pipeline ${cmd.pipeline} (the manifest has ${this.#pipelines.length})`);
    }
    // A tier-1 pipeline a command needs before its tier's turn starts building now.
    if ("pipeline" in cmd && !this.#ready(cmd)) {
      this.#start(cmd.pipeline);
      this.waits.push({ frame: this.#frame, pipeline: cmd.pipeline, at: performance.now() });
    }
    const queue = this.#deferred ?? (this.#waitingAt < this.#waiting.length || !this.#ready(cmd) ? this.#waiting : null);
    if (queue !== null) queue.push({ cmd: copied(cmd), at: { frame: this.#frame, generation: this.#generation } });
    else this.#run(cmd);
  }

  /** Whether a command waits for a pipeline being built. Throws why the program failed, if a
   * pipeline couldn't be built or a command that waited for one failed. */
  get waiting(): boolean {
    if (this.#failed !== null) throw this.#failed;
    return this.#waitingAt < this.#waiting.length;
  }

  /** The program has failed, as `e` says (`#failed`). */
  #fail(e: unknown): void {
    if (this.#retired || this.#failed !== null) return;
    this.#failed = e;
    this.#rejectAll(e);
  }

  /** Drops every queued command and rejects every queued readback, and `caughtUp`, with `e`. */
  #rejectAll(e: unknown): void {
    this.#rejectDeferred(this.#waiting, e);
    if (this.#deferred !== null) this.#rejectDeferred(this.#deferred, e);
    for (const batch of this.#draining) this.#rejectDeferred(batch, e);
    this.#idle?.reject(e);
    this.#idle = null;
  }

  /** Drops queued commands and rejects queued readbacks with `e`. */
  #rejectDeferred(queue: (Deferred | undefined)[], e: unknown): void {
    for (const d of queue.splice(0)) if (d !== undefined && !("cmd" in d)) d.answer.reject(e);
    if (queue === this.#waiting) this.#waitingAt = 0;
  }

  /** Resolves once no command waits for a pipeline being built (at once, if none does); rejects
   * if a pipeline couldn't be built. */
  caughtUp(): Promise<void> {
    if (this.#failed !== null) return Promise.reject(this.#failed);
    if (this.#waitingAt === this.#waiting.length) return Promise.resolve();
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
    if (this.#retired || this.#failed !== null || this.#waitingAt === this.#waiting.length) return;
    while (this.#waitingAt < this.#waiting.length) {
      const d = this.#waiting[this.#waitingAt]!;
      if ("cmd" in d && !this.#ready(d.cmd)) return;
      this.#waiting[this.#waitingAt++] = undefined;
      if ("cmd" in d) this.#runAs(d.at, d.cmd);
      else this.readBack(d.handle, d.offset, d.size, true).then(d.answer.resolve, d.answer.reject);
    }
    this.#waiting.length = 0;
    this.#waitingAt = 0;
    this.flush();
    this.#idle?.resolve();
    this.#idle = null;
  }

  /** Runs `cmd` as part of the frame it was recorded in (`at`): its timings and its label are
   * that frame's. */
  #runAs(at: RecordedIn, cmd: Command): void {
    this.#running = at;
    try {
      this.#run(cmd, true);
    } finally {
      this.#running = null;
    }
  }

  /** The serial mode: runs the commands kept since the last call, submitting each pass and
   * dispatch alone and waiting for the GPU to finish it before the next. */
  async drain(): Promise<void> {
    if (this.#deferred === null) return;
    const batch = this.#deferred.splice(0);
    this.#draining.add(batch);
    try {
      for (const d of batch) {
        if (this.#retired) return;
        if (this.#failed !== null) throw this.#failed;
        if ("cmd" in d && "pipeline" in d.cmd) await this.#building[d.cmd.pipeline];
        if (this.#retired) return;
        if (this.#failed !== null) throw this.#failed;
        if ("cmd" in d) {
          // Its timings and its label are the frame's that recorded it, which may have been
          // before the one being recorded now.
          try {
            this.#runAs(d.at, d.cmd);
          } catch (e) {
            throw new Error(`serial mode, ${d.cmd.op}: ${errorMessage(e)}`);
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
    } catch (error) {
      this.#fail(error);
      throw error;
    } finally {
      this.#draining.delete(batch);
      if (this.#asked.length > 0) this.#answerTimes();
    }
  }

  #run(cmd: Command, ownedUniforms = false): void {
    // A label names the passes and dispatches after it in its own frame: the first command of
    // another frame starts with none.
    const generation = this.#running?.generation ?? this.#generation;
    if (generation !== this.#labelGeneration) {
      this.#label = null;
      this.#labelGeneration = generation;
    }
    if (this.#ended !== null) {
      const mayJoin = cmd.op === "BeginPass" && generation === this.#ended.at.generation && joins(cmd.pass, this.#ended.pass);
      // Keep both passes until their draws say whether their resource usage scopes can
      // share one WebGPU pass. A kept target alone doesn't make read/write uses compatible.
      // A `Label` records it too, since a pass joins only one of its own label.
      if (!mayJoin && this.#pass === null) {
        this.#recordEnded();
      }
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
        // A 3D texture (`cmd.layers` texels deep) is sampled and written by kernels, never drawn
        // into; a 2D texture, an array and a cube are 2D textures of `cmd.layers` layers.
        const three = cmd.kind === "3d";
        const info = formatOf(cmd.format);
        const renderable = !three && can(this.device, info.renderable, info.needs.render);
        const texture = this.device.createTexture({
          label: `texture ${cmd.handle}`,
          size: [cmd.width, cmd.height, cmd.layers],
          dimension: three ? "3d" : "2d",
          mipLevelCount: cmd.levels,
          format: cmd.format as GPUTextureFormat,
          usage:
            GPUTextureUsage.TEXTURE_BINDING |
            (renderable ? GPUTextureUsage.RENDER_ATTACHMENT : 0) |
            GPUTextureUsage.COPY_SRC |
            (depth ? 0 : GPUTextureUsage.COPY_DST) |
            // Only where kernels write it: a storage texture may give up the GPU's compression.
            (cmd.writable ? GPUTextureUsage.STORAGE_BINDING : 0),
          // A cube's whole view is a cube (WebGPU's compatibility mode asks it be said).
          ...(cmd.kind === "cube" ? { textureBindingViewDimension: "cube" as GPUTextureViewDimension } : {}),
        });
        let bytes = 0;
        for (let l = 0; l < cmd.levels; l++) bytes += bytesOf(cmd.format, ...levelSize(cmd.width, cmd.height, l));
        bytes *= cmd.layers;
        this.#resources.set(cmd.handle, { kind: "texture", texture, format: cmd.format, textureKind: cmd.kind, levels: cmd.levels, bytes, views: new Map() });
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
          mipmapFilter: cmd.mipLinear ? "linear" : "nearest",
          maxAnisotropy: cmd.anisotropy,
          ...(cmd.compare === null ? {} : { compare: cmd.compare }),
        });
        this.#resources.set(cmd.handle, { kind: "sampler", sampler });
        return;
      }
      case "WriteBuffer":
        this.#write(cmd.handle, cmd.offset, cmd.data);
        return;
      case "WriteTexture": {
        this.#writeTexture(cmd);
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
        this.#begin(onePass(SCREEN, false, cmd.clear));
        return;
      case "BeginPass":
        this.#begin(cmd.pass);
        return;
      case "Draw":
      case "DrawIndirect":
      case "DrawIndexed":
      case "DrawIndexedIndirect":
      case "MultiDrawIndirect":
      case "MultiDrawIndexedIndirect":
        // Immediate commands view the program's memory; queued commands already own a copy.
        this.#pass?.draws.push({ ...cmd, uniforms: ownedUniforms ? cmd.uniforms : cmd.uniforms.slice() });
        return;
      case "SetScissor":
      case "SetViewport":
        this.#pass?.draws.push(cmd);
        return;
      case "ConfigureScreen": {
        const show = this.screen.show;
        if (show === undefined) return;
        const format: GPUTextureFormat = cmd.hdr ? "rgba16float" : show.sdr;
        const want = (cmd.hdr ? 1 : 0) | (cmd.gamut === 1 ? 2 : 0);
        // Asked again for what it shows, or what it's changing to: nothing to do.
        if (want === (this.#screenWanted ?? this.screenState)) return;
        this.#screenWanted = want;
        const asked = ++this.#screenAsked;
        // The frames go on in the old format until every pipeline that draws the screen can
        // draw it in the new one; then, between frames, the canvas changes.
        this.screenVariants(format).then(() => {
          if (this.#retired || asked !== this.#screenAsked) return;
          this.#screenWanted = undefined;
          if (show.apply(format, cmd.hdr, cmd.gamut)) {
            this.screen.format = format;
            this.screen.state = want;
          }
        }, (e: unknown) => this.#fail(e));
        return;
      }
      case "CopyTexture": {
        const { source: s, destination: d } = cmd;
        const from = this.#texture(s.texture);
        this.#encoderNow().copyTextureToTexture(
          { texture: from.texture, mipLevel: s.level, origin: { x: s.x, y: s.y, z: s.layer } },
          { texture: this.#texture(d.texture).texture, mipLevel: d.level, origin: { x: d.x, y: d.y, z: d.layer } },
          wholeBlocks(from.format, s.width, s.height),
        );
        return;
      }
      case "CopyTextureToBuffer":
      case "CopyBufferToTexture": {
        const r = cmd.op === "CopyTextureToBuffer" ? cmd.source : cmd.destination;
        const t = this.#texture(r.texture);
        const rows = Math.ceil(r.height / blockOf(t.format));
        const image = { texture: t.texture, mipLevel: r.level, origin: { x: r.x, y: r.y, z: r.layer } };
        const buffer = { buffer: this.#buffer(cmd.buffer), offset: cmd.offset, bytesPerRow: cmd.bytesPerRow, rowsPerImage: rows };
        const size = wholeBlocks(t.format, r.width, r.height);
        if (cmd.op === "CopyTextureToBuffer") this.#encoderNow().copyTextureToBuffer(image, buffer, size);
        else this.#encoderNow().copyBufferToTexture(buffer, image, size);
        return;
      }
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
      case "Timed": {
        // The ask times its label's work from here on in the stream (`timeLabel`). Before a
        // timer, nothing has been timed, and a timer starts its count at 0.
        const from = this.#timer?.noted ?? 0;
        const ask = this.#asked.find((a) => a.from === null && a.label === cmd.label);
        const early = this.#early.get(cmd.label);
        if (ask !== undefined) ask.from = from;
        else if (early !== undefined) early.push(from);
        else this.#early.set(cmd.label, [from]);
        return;
      }
      case "ReadBuffer":
      case "StorageRead":
      case "StorageWrite":
      case "Fetch":
        return;
    }
  }

  /** Opens `pass`, named by the label in force, in the frame of the command running. */
  #begin(pass: Pass): void {
    this.#pass = { pass, draws: [], label: this.#label, at: this.#running ?? { frame: this.#frame, generation: this.#generation } };
  }

  /** Submits everything recorded so far, after writing its uniform bytes, then destroys the
   * resources the program destroyed (WebGPU waits for submitted work that uses them). */
  flush(): void {
    this.#recordEnded();
    this.#endCompute();
    if (this.#encoder !== null) {
      this.#uniforms.write(this.device.queue, this.#counters);
      this.#uploads.write(this.device.queue, this.#counters);
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
    if (this.#retired) return Promise.reject(new Error("the GPU executor was retired"));
    const queue = this.#deferred ?? (this.#waitingAt < this.#waiting.length ? this.#waiting : null);
    if (queue !== null && !now) {
      const answer = Promise.withResolvers<Bytes>();
      queue.push({ handle, offset, size, answer });
      return answer.promise;
    }
    this.flush();
    const source = this.#buffer(handle);
    const staging = this.#readbackBuffer(Math.max(size, 4));
    return this.#copyBack(source, offset, size, staging).finally(() => staging.destroy());
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

  /** What it did since the last take (test mode's `counters.json`, a frame each). */
  takeCounters(): Counters {
    const out = this.#counters;
    this.#counters = noCounters();
    return out;
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
    if (this.#retired) return;
    this.#retired = true;
    this.#rejectAll(new Error("the GPU executor was retired"));
    this.#pass = null;
    // An ended pass still needs the live attachment and binding handles to be recorded.
    this.#recordEnded();
    for (const handle of [...this.#resources.keys()]) this.#destroy(handle);
    this.flush();
    this.#uniforms.buffer.destroy();
    this.#uploads.buffer.destroy();
    this.#timer?.destroy();
    if (this.#debugFlag !== null) {
      this.#debugFlag.buffer.destroy();
      if (!this.#debugReading) this.#debugFlag.readback.destroy();
    }
  }

  #destroy(handle: number): void {
    const r = this.#resources.get(handle);
    if (r === undefined) throw new Error(`host bug: resource ${handle} passed the checks but doesn't exist`);
    // Recorded work may still use it: it's destroyed once that work is submitted.
    if (r.kind === "buffer") {
      this.#doomed.push(r.buffer);
      this.#allocated(-r.buffer.size);
    } else if (r.kind === "texture") {
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

  /** A texture pass's targets' size: its first colour target's level, or its depth's. */
  #passSize(pass: Pass): [number, number] {
    const c = pass.colors[0];
    const t = this.#texture(c === undefined ? pass.depth : c.texture).texture;
    return levelSize(t.width, t.height, c?.level ?? 0);
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
      this.#counters.write_buffers++;
      this.#counters.write_buffer_bytes += len;
      return;
    }
    const at = this.#reserveUpload(len, 4);
    const up = this.#uploads;
    // The bytes are a view of the batch the program submitted: staged, so a frame's uploads are
    // one queue write. (Writing a big one to the ring straight from the batch saved nothing
    // measurable on the Last Green, §A13 of the world-class plan, and cost queue calls.)
    up.bytes.set(data, at);
    this.#encoderNow().copyBufferToBuffer(up.buffer, at, this.#buffer(handle), offset, len);
  }

  /** An aligned slice of the upload ring; flushes or grows it before recording any copy. */
  #reserveUpload(len: number, align: number): number {
    const up = this.#uploads;
    let at = alignTo(up.used, align);
    if (at + len > up.bytes.length) {
      this.flush();
      if (len > up.bytes.length) up.grow(len);
      at = 0;
    }
    up.used = at + len;
    return at;
  }

  /** Texture copies share the buffer upload ring, with the row alignment WebGPU requires. Rows
   * of a compressed format are rows of blocks. */
  #writeTexture(cmd: Extract<Command, { op: "WriteTexture" }>): void {
    if (cmd.data.length === 0) return;
    const t = this.#texture(cmd.handle);
    const target = { texture: t.texture, mipLevel: cmd.level, origin: { x: cmd.x, y: cmd.y, z: cmd.layer } };
    const size = wholeBlocks(t.format, cmd.width, cmd.height);
    const rows = Math.ceil(cmd.height / blockOf(t.format));
    const row = cmd.data.length / rows;
    const padded = alignTo(row, 256);
    const len = padded * rows;
    if (len > UPLOAD_MAX) {
      this.flush();
      this.device.queue.writeTexture(target, cmd.data, { bytesPerRow: row, rowsPerImage: rows }, size);
      this.#counters.write_textures++;
      return;
    }
    const at = this.#reserveUpload(len, 256);
    const up = this.#uploads;
    if (row === padded) up.bytes.set(cmd.data, at);
    else {
      for (let y = 0; y < rows; y++) {
        up.bytes.set(cmd.data.subarray(y * row, (y + 1) * row), at + y * padded);
      }
    }
    this.#encoderNow().copyBufferToTexture(
      { buffer: up.buffer, offset: at, bytesPerRow: padded, rowsPerImage: rows },
      target,
      size,
    );
  }

  /** The encoder, for anything but a dispatch: a run of dispatches' compute pass ends first. */
  #encoderNow(): GPUCommandEncoder {
    this.#endCompute();
    this.#encoder ??= this.device.createCommandEncoder();
    return this.#encoder;
  }

  #endCompute(): void {
    if (this.#compute === null) return;
    this.#compute.pass.end();
    this.#compute = null;
  }

  /** The compute pass for a dispatch: untimed, the open one a run of dispatches of one label shares (each
   * dispatch is its own usage scope inside it, so WebGPU orders their memory as across passes);
   * timed, a pass of its own for its timestamps. */
  #computePass(label: string, timed: { timestampWrites?: GPUComputePassTimestampWrites }): OpenCompute {
    // A label names its passes: a dispatch under another opens its own.
    if (this.#compute !== null && this.#compute.label === label && timed.timestampWrites === undefined) return this.#compute;
    const pass = this.#encoderNow().beginComputePass({ label, ...timed });
    this.#counters.compute_passes++;
    this.#compute = { pass, label, pipeline: null, group: null, offsets: [] };
    return this.#compute;
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
      this.#lastGroups.fill(undefined);
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
    const last = this.#lastGroups[index];
    if (last !== undefined && last.ranges.length === bindings.length * 3) {
      let same = true;
      for (let i = 0; i < bindings.length; i++) {
        const b = bindings[i]!;
        if (last.ranges[3 * i] !== b.handle || last.ranges[3 * i + 1] !== b.offset || last.ranges[3 * i + 2] !== b.size) {
          same = false;
          break;
        }
      }
      if (same) return last.group;
    }
    // The pipeline, then each binding's range: a key is needed only when its ranges change.
    let key = `${index}:`;
    for (let i = 0; i < bindings.length; i++) {
      const b = bindings[i]!;
      if (i > 0) key += ",";
      key += b.handle + "@" + b.offset + "+" + b.size;
    }
    const cached = this.#bindGroups.get(key);
    if (cached !== undefined) {
      this.#lastGroups[index] = cached;
      return cached.group;
    }
    const p = this.#pipeline(index);
    const entries: GPUBindGroupEntry[] = [];
    if (p.uniform !== null) {
      entries.push({ binding: p.uniform.binding, resource: { buffer: this.#uniforms.buffer, offset: 0, size: p.uniform.size } });
    }
    bindings.forEach((b, i) => {
      const r = this.#resources.get(b.handle);
      if (r === undefined) throw new Error(`host bug: resource ${b.handle} passed the checks but doesn't exist`);
      const slot = p.bindings[i]!;
      const resource: GPUBindingResource =
        r.kind === "buffer"
          ? { buffer: r.buffer, offset: b.offset, size: b.size }
          : r.kind === "texture"
            ? viewOf(r, b.offset === 0 ? null : b.offset - 1, b.size === 0 ? null : b.size - 1, dimensionOf(slot.kind) as GPUTextureViewDimension, isStorage(slot.kind))
            : r.sampler;
      entries.push({ binding: slot.binding, resource });
    });
    if (p.debugFlag !== null && this.#debugFlag !== null) {
      entries.push({ binding: p.debugFlag, resource: { buffer: this.#debugFlag.buffer } });
    }
    const group = this.device.createBindGroup({ label: p.name, layout: p.layout, entries });
    const handles = bindings.map((b) => b.handle);
    const ranges: number[] = [];
    for (const b of bindings) ranges.push(b.handle, b.offset, b.size);
    const entry = { group, pipeline: index, handles, ranges };
    this.#bindGroups.set(key, entry);
    this.#lastGroups[index] = entry;
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
    if (this.#lastGroups[cached.pipeline] === cached) this.#lastGroups[cached.pipeline] = undefined;
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
    const c = this.#computePass(label, timed);
    const pass = c.pass;
    // What the pass has set already isn't set again.
    if (c.pipeline !== p.compute) {
      pass.setPipeline(p.compute);
      c.pipeline = p.compute;
      this.#counters.pipelines++;
    }
    if (c.group !== bindGroup || !sameOffsets(c.offsets, offsets)) {
      pass.setBindGroup(0, bindGroup, offsets);
      c.group = bindGroup;
      c.offsets = offsets;
      this.#counters.bind_groups++;
    }
    this.#counters.dispatches++;
    if (Array.isArray(groups)) pass.dispatchWorkgroups(groups[0], groups[1], groups[2]);
    else pass.dispatchWorkgroupsIndirect(this.#buffer(groups.buffer), groups.offset);
    // A timed dispatch's pass ends with it, its timestamps around it alone.
    if (timed.timestampWrites !== undefined) this.#endCompute();
  }

  /** The render pipeline of `index` for the targets `key` names (`targetsKey`): one of the
   * manifest's for it, which the checker checked, made at load. */
  #variant(index: number, key: string): GPURenderPipeline {
    const v = this.#pipeline(index).render?.variants.get(key);
    if (v === undefined) throw new Error(`host bug: pipeline ${index} has no variant for its pass's targets`);
    return v;
  }

  /** Ends the open pass: on the screen (Present) it's recorded; into textures (EndPass), it
   * waits for the next command, which may continue it (`joins`, the same label), unless the
   * mode is serial. A pass joins only one of its own label, so its time is that label's. */
  #endPass(op: OpcodeName): void {
    const open = this.#pass;
    this.#pass = null;
    if (open === null) throw new Error(`host bug: ${op} without a pass`);
    const ended = this.#ended;
    if (ended !== null) {
      const sameFrame = open.at.generation === ended.at.generation;
      if (op === "EndPass" && sameFrame && open.label === ended.label && joins(open.pass, ended.pass) && this.#compatible(ended, open)) {
        // A pass begins with the whole target its scissor and viewport: where the one it joins
        // set either, the joined draws start from the whole target again.
        if (ended.draws.some((d) => !isDraw(d))) {
          const [width, height] = this.#passSize(ended.pass);
          ended.draws.push({ op: "SetScissor", x: 0, y: 0, width, height });
          ended.draws.push({ op: "SetViewport", rect: [0, 0, width, height], minDepth: 0, maxDepth: 1 });
        }
        for (const draw of open.draws) ended.draws.push(draw);
        return;
      }
      this.#recordEnded();
    }
    if (op === "EndPass" && this.#deferred === null) {
      this.#ended = open;
      return;
    }
    this.#recordPass(op, open);
  }

  /** A combined render pass cannot use a resource both read-only and read-write, including
   * reads of indirect arguments and indices. Each individual pass was already checked. */
  #compatible(earlier: OpenPass, next: OpenPass): boolean {
    const usage = earlier.uses ?? new Map<number, number>();
    const use = (handle: number, access: number) => {
      const combined = (usage.get(handle) ?? 0) | access;
      usage.set(handle, combined);
      return combined !== 3;
    };
    const lists = earlier.uses ? [next.draws] : [earlier.draws, next.draws];
    for (const draws of lists) for (const draw of draws) {
      if (!isDraw(draw)) continue;
      const pipeline = this.#pipeline(draw.pipeline);
      for (const [i, binding] of draw.bindings.entries()) {
        const kind = pipeline.bindings[i]!.kind;
        if (kind !== "sampler" && kind !== "comparison_sampler" && kind !== "non_filtering_sampler") {
          if (!use(binding.handle, kind === "read_write" || isStorage(kind) ? 2 : 1)) return false;
        }
      }
      const args = argumentsOf(draw);
      if (args !== null && !use(args, 1)) return false;
      const indices = indicesOf(draw);
      if (indices !== null && !use(indices, 1)) return false;
    }
    earlier.uses = usage;
    return true;
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
    const screen = onScreen(pass);
    const colors = pass.colors.map((c) => {
      if (c.texture === SCREEN) return { c, view: this.screen.texture().createView(), format: this.screen.format };
      const t = this.#texture(c.texture);
      return { c, view: viewOf(t, c.level, c.layer, "2d", false), format: t.format as GPUTextureFormat };
    });
    const depth = pass.depth === NONE ? null : this.#texture(pass.depth);
    const depthView = depth === null ? null : viewOf(depth, 0, pass.depthLayer, "2d", false);
    const key = targetsKey(colors.map((c) => c.format), depth?.format ?? null);
    // Everything that can flush happens before anything is recorded for this pass.
    this.#reserveTimestamps();
    this.#reserve(op, open.draws.reduce((n, d) => n + (isDraw(d) ? alignTo(d.uniforms.length, this.#align) : 0), 0));
    // And everything that can throw: a pass begun is ended.
    const draws = open.draws.map((d) => {
      if (!isDraw(d)) return { draw: d, pipeline: null, group: null, args: null, indices: null };
      const i = indicesOf(d);
      const indices = i === null ? null : this.#buffer(i);
      const a = argumentsOf(d);
      return {
        draw: d,
        pipeline: this.#variant(d.pipeline, key),
        group: this.#bindGroup(d.pipeline, d.bindings),
        args: a === null ? null : this.#buffer(a),
        indices,
      };
    });
    const label = open.label ?? (screen ? "screen pass" : "pass");
    const timed = this.#timestamps(label, open.at.frame);
    const encoder = this.#encoderNow();
    const rp = encoder.beginRenderPass({
      label,
      ...timed,
      colorAttachments: colors.map(({ c, view }) => {
        const [r, g, b, a] = c.clear;
        return { view, clearValue: { r, g, b, a }, loadOp: c.keep ? "load" : "clear", storeOp: "store" };
      }),
      ...(depthView === null
        ? {}
        : {
            depthStencilAttachment: {
              view: depthView,
              depthClearValue: pass.clearDepth,
              depthLoadOp: pass.keepDepth ? "load" : "clear",
              depthStoreOp: "store",
            },
          }),
    });
    // The device's multi-draw, which WebGPU's types don't have yet; null without it.
    const maybe = rp as Partial<MultiDrawPass>;
    const multi = this.#multiDraw && maybe.multiDrawIndirect && maybe.multiDrawIndexedIndirect ? (rp as MultiDrawPass) : null;
    const counted = this.#counters;
    counted.render_passes++;
    let lastPipeline: GPURenderPipeline | null | undefined;
    let lastGroup: GPUBindGroup | null | undefined;
    let lastOffsets: number[] = [];
    for (const { draw: d, pipeline, group, args, indices } of draws) {
      if (d.op === "SetScissor") {
        rp.setScissorRect(d.x, d.y, d.width, d.height);
        continue;
      }
      if (d.op === "SetViewport") {
        rp.setViewport(d.rect[0], d.rect[1], d.rect[2], d.rect[3], d.minDepth, d.maxDepth);
        continue;
      }
      const offsets = this.#push(d.uniforms);
      // What the pass has set already isn't set again: consecutive draws of a pipeline, and of a
      // bind group at the same offsets.
      if (pipeline !== lastPipeline) {
        rp.setPipeline(pipeline!);
        lastPipeline = pipeline;
        counted.pipelines++;
      }
      if (group !== lastGroup || !sameOffsets(lastOffsets, offsets)) {
        rp.setBindGroup(0, group!, offsets);
        lastGroup = group;
        lastOffsets = offsets;
        counted.bind_groups++;
      }
      const many = d.op === "MultiDrawIndirect" || d.op === "MultiDrawIndexedIndirect";
      counted.draws += many ? d.count : 1;
      counted.draw_calls += many && multi === null ? d.count : 1;
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
        case "DrawIndexed":
          rp.setIndexBuffer(indices!, "uint32", d.index_offset, d.index_size);
          rp.drawIndexed(d.index_count, d.instances, d.first_index, d.base_vertex, d.first_instance);
          break;
        case "MultiDrawIndirect":
        case "MultiDrawIndexedIndirect": {
          const indexed = d.indices !== null;
          if (indexed) rp.setIndexBuffer(indices!, "uint32", d.indices![1], d.indices![2]);
          // With the device's multi-draw, one call; without it, one draw each, which draws the
          // same (the program doesn't change).
          if (multi !== null && indexed) multi.multiDrawIndexedIndirect(args!, d.offset, d.count);
          else if (multi !== null) multi.multiDrawIndirect(args!, d.offset, d.count);
          else {
            const stride = indexed ? 20 : 16;
            for (let k = 0; k < d.count; k++) {
              if (indexed) rp.drawIndexedIndirect(args!, d.offset + k * stride);
              else rp.drawIndirect(args!, d.offset + k * stride);
            }
          }
          break;
        }
      }
    }
    rp.end();
    if (screen) {
      this.screen.afterPass?.(encoder);
      this.flush();
    }
  }
}

/** A render pass with Chromium's multi-draw ("chromium-experimental-multi-draw-indirect"). */
type MultiDrawPass = GPURenderPassEncoder & {
  multiDrawIndirect(b: GPUBuffer, o: number, n: number): void;
  multiDrawIndexedIndirect(b: GPUBuffer, o: number, n: number): void;
};

/** A pass between its beginning and `Present` or `EndPass`, its name and the frame it belongs to. */
type OpenPass = { pass: Pass; draws: PendingDraw[]; label: string | null; at: RecordedIn; uses?: Map<number, number> };
