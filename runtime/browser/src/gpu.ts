// The GPU side: builds the manifest's pipelines, and carries out decoded, sequenced and checked
// commands with WebGPU. The same mapping as the native host (runtime/native/src/gpu.rs):
//
// - Work is recorded into one pending command encoder and submitted ("flushed") at Present,
//   before a WriteBuffer (so earlier dispatches see the old contents), and at the end of each
//   frame. Each dispatch gets its own compute pass.
// - A screen pass's draws are collected and recorded into one render pass at Present, since a
//   pass can span batches.
// - Uniform bytes go into a ring buffer bound with dynamic offsets (256-byte aligned by
//   default), staged on the CPU and written just before each flush, so every command in a
//   submission has its own slice. It grows (after a flush) when a submission needs more.
// - Bind group 0 of each pipeline is laid out from the manifest: the uniform block (dynamic
//   offset), then the buffers in order. Bind groups are cached per (pipeline, buffer handles).

import { SCREEN_FORMAT } from "./abi.gen.ts";
import { errorMessage } from "./errors.ts";
import type { BufferBinding, Manifest, Pipeline } from "./manifest.ts";
import type { Bytes, Command } from "./stream.ts";

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

export interface BuiltPipeline {
  name: string;
  layout: GPUBindGroupLayout;
  compute: GPUComputePipeline | null;
  render: GPURenderPipeline | null;
  uniform: { binding: number; size: number } | null;
  buffers: BufferBinding[];
}

/** Where screen passes draw. */
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
  for (const b of p.buffers) {
    const readOnly = b.access === "read";
    entries.push({
      binding: b.binding,
      // WebGPU forbids writable storage in vertex shaders.
      visibility: readOnly || p.kind === "compute" ? visibility : GPUShaderStage.FRAGMENT,
      buffer: { type: readOnly ? "read-only-storage" : "storage" },
    });
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
  let render: GPURenderPipeline | null = null;
  try {
    if (p.kind === "compute") {
      compute = await device.createComputePipelineAsync({
        label: p.name,
        layout: pipelineLayout,
        compute: { module, entryPoint: p.entry },
      });
    } else {
      render = await device.createRenderPipelineAsync({
        label: p.name,
        layout: pipelineLayout,
        vertex: { module, entryPoint: p.vertex_entry, buffers: [] },
        // Triangle list, counter-clockwise front faces, no culling (the defaults).
        primitive: { topology: "triangle-list" },
        fragment: { module, entryPoint: p.fragment_entry, targets: [{ format: SCREEN_FORMAT }] },
      });
    }
  } catch (e) {
    throw fail(errorMessage(e));
  }
  return {
    name: p.name,
    layout,
    compute,
    render,
    uniform: p.uniform && { binding: p.uniform.binding, size: p.uniform.size },
    buffers: p.buffers,
  };
}

/** Builds every pipeline up front. `shaders[i]` is pipeline `i`'s WGSL. */
export function buildPipelines(device: GPUDevice, manifest: Manifest, shaders: string[]): Promise<BuiltPipeline[]> {
  return Promise.all(manifest.pipelines.map((p, i) => buildPipeline(device, p, shaders[i]!)));
}

interface PendingDraw {
  pipeline: number;
  vertices: number;
  instances: number;
  buffers: number[];
  uniforms: Bytes;
}

/** `n` rounded up to a multiple of `align`. */
export const alignTo = (n: number, align: number) => Math.ceil(n / align) * align;

export class GpuExecutor {
  readonly #align: number;
  readonly #buffers = new Map<number, GPUBuffer>();
  /** By pipeline and buffer handles. */
  readonly #bindGroups = new Map<string, { group: GPUBindGroup; handles: number[] }>();
  /** The keys of the bind groups that use each buffer, so DestroyBuffer drops only those. */
  readonly #groupsOf = new Map<number, Set<string>>();
  /** Buffers destroyed by the program, released once the work recorded before is submitted. */
  readonly #doomed: GPUBuffer[] = [];
  #ring: GPUBuffer;
  #staging: Bytes = new Uint8Array(RING_START);
  #staged = 0;
  #encoder: GPUCommandEncoder | null = null;
  #pass: { clear: [number, number, number, number]; draws: PendingDraw[] } | null = null;

  constructor(
    readonly device: GPUDevice,
    readonly pipelines: BuiltPipeline[],
    readonly screen: ScreenTarget,
  ) {
    const l = device.limits;
    this.#align = Math.max(l.minUniformBufferOffsetAlignment, l.minStorageBufferOffsetAlignment);
    this.#ring = this.#createRing(RING_START);
  }

  /** Carries out a command that has been decoded, sequenced and checked. */
  execute(cmd: Command): void {
    switch (cmd.op) {
      case "CreateBuffer":
        this.#buffers.set(
          cmd.handle,
          this.device.createBuffer({
            label: `buffer ${cmd.handle}`,
            size: cmd.size,
            usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST | GPUBufferUsage.COPY_SRC,
          }),
        );
        return;
      case "WriteBuffer":
        if (cmd.data.length === 0) return;
        // Dispatches recorded before this write must see the old contents.
        this.flush();
        this.device.queue.writeBuffer(this.#buffer(cmd.handle), cmd.offset, cmd.data);
        return;
      case "Dispatch":
        this.#dispatch(cmd.pipeline, cmd.groups, cmd.buffers, cmd.uniforms);
        return;
      case "BeginScreenPass":
        this.#pass = { clear: cmd.clear, draws: [] };
        return;
      case "Draw": {
        // The uniform bytes are a view into the program's memory: copy them.
        const { pipeline, vertices, instances, buffers } = cmd;
        this.#pass?.draws.push({ pipeline, vertices, instances, buffers, uniforms: cmd.uniforms.slice() });
        return;
      }
      case "Present":
        this.#present();
        return;
      case "DestroyBuffer": {
        // Recorded work may still use it: it's destroyed once that work is submitted.
        this.#doomed.push(this.#buffer(cmd.handle));
        this.#buffers.delete(cmd.handle);
        for (const key of this.#groupsOf.get(cmd.handle) ?? []) this.#dropBindGroup(key);
        this.#groupsOf.delete(cmd.handle);
        return;
      }
    }
  }

  /** Submits everything recorded so far, after writing its uniform bytes, then destroys the
   * buffers the program destroyed (WebGPU waits for submitted work that uses them). */
  flush(): void {
    if (this.#encoder !== null) {
      if (this.#staged > 0) {
        this.device.queue.writeBuffer(this.#ring, 0, this.#staging, 0, this.#staged);
        this.#staged = 0;
      }
      this.device.queue.submit([this.#encoder.finish()]);
      this.#encoder = null;
    }
    for (const b of this.#doomed.splice(0)) b.destroy();
  }

  #buffer(handle: number): GPUBuffer {
    const b = this.#buffers.get(handle);
    if (b === undefined) throw new Error(`host bug: buffer ${handle} passed the checks but doesn't exist`);
    return b;
  }

  #pipeline(index: number): BuiltPipeline {
    const p = this.pipelines[index];
    if (p === undefined) throw new Error(`host bug: pipeline ${index} passed the checks but doesn't exist`);
    return p;
  }

  #createRing(capacity: number): GPUBuffer {
    return this.device.createBuffer({
      label: "uniform ring",
      size: capacity,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
    });
  }

  #encoderNow(): GPUCommandEncoder {
    this.#encoder ??= this.device.createCommandEncoder();
    return this.#encoder;
  }

  /** Makes room for `bytes` more uniform bytes (counted aligned) in this submission, flushing
   * first or growing the ring if it must. Call before recording the work that uses them. */
  #reserve(bytes: number): void {
    if (alignTo(this.#staged, this.#align) + bytes <= this.#staging.length) return;
    this.flush();
    if (bytes > this.#staging.length) {
      let capacity = this.#staging.length * 2;
      while (capacity < bytes) capacity *= 2;
      if (capacity > this.device.limits.maxBufferSize) {
        throw new Error(`${bytes} uniform bytes in one submission is over the limit`);
      }
      this.#ring.destroy(); // the flush above submitted the last work that used it
      this.#ring = this.#createRing(capacity);
      this.#staging = new Uint8Array(capacity);
      this.#bindGroups.clear(); // they point at the old ring
      this.#groupsOf.clear();
    }
  }

  /** Appends uniform bytes to this submission's slice of the ring; returns their offsets. */
  #push(bytes: Bytes): number[] {
    if (bytes.length === 0) return [];
    const offset = alignTo(this.#staged, this.#align);
    this.#staging.fill(0, this.#staged, offset);
    this.#staging.set(bytes, offset);
    this.#staged = offset + bytes.length;
    return [offset];
  }

  #bindGroup(index: number, handles: number[]): GPUBindGroup {
    const key = `${index}:${handles.join(",")}`;
    const cached = this.#bindGroups.get(key);
    if (cached !== undefined) return cached.group;
    const p = this.#pipeline(index);
    const entries: GPUBindGroupEntry[] = [];
    if (p.uniform !== null) {
      entries.push({ binding: p.uniform.binding, resource: { buffer: this.#ring, offset: 0, size: p.uniform.size } });
    }
    handles.forEach((h, i) => entries.push({ binding: p.buffers[i]!.binding, resource: { buffer: this.#buffer(h) } }));
    const group = this.device.createBindGroup({ label: p.name, layout: p.layout, entries });
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

  #dispatch(index: number, groups: [number, number, number], buffers: number[], uniforms: Bytes): void {
    const p = this.#pipeline(index);
    if (p.compute === null) throw new Error(`host bug: pipeline ${index} isn't a compute pipeline`);
    this.#reserve(alignTo(uniforms.length, this.#align));
    const offsets = this.#push(uniforms);
    const bindGroup = this.#bindGroup(index, buffers);
    const pass = this.#encoderNow().beginComputePass({ label: p.name });
    pass.setPipeline(p.compute);
    pass.setBindGroup(0, bindGroup, offsets);
    pass.dispatchWorkgroups(groups[0], groups[1], groups[2]);
    pass.end();
  }

  #present(): void {
    const pass = this.#pass;
    this.#pass = null;
    if (pass === null) throw new Error("host bug: Present without a screen pass");
    this.#reserve(pass.draws.reduce((n, d) => n + alignTo(d.uniforms.length, this.#align), 0));
    const recorded = pass.draws.map((d) => {
      const p = this.#pipeline(d.pipeline);
      if (p.render === null) throw new Error(`host bug: pipeline ${d.pipeline} isn't a render pipeline`);
      return { d, render: p.render, offsets: this.#push(d.uniforms), bindGroup: this.#bindGroup(d.pipeline, d.buffers) };
    });
    const [r, g, b, a] = pass.clear;
    const encoder = this.#encoderNow();
    const rp = encoder.beginRenderPass({
      label: "screen pass",
      colorAttachments: [
        { view: this.screen.texture().createView(), clearValue: { r, g, b, a }, loadOp: "clear", storeOp: "store" },
      ],
    });
    for (const { d, render, offsets, bindGroup } of recorded) {
      rp.setPipeline(render);
      rp.setBindGroup(0, bindGroup, offsets);
      rp.draw(d.vertices, d.instances);
    }
    rp.end();
    this.screen.afterPass?.(encoder);
    this.flush();
  }
}
