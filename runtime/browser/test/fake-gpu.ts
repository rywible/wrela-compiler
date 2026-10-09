// A fake WebGPU device that records what the runtime asks of it, for testing the decoder's
// mapping to WebGPU without a GPU. Only what the runtime uses is implemented.

export interface FakeBuffer {
  label: string;
  size: number;
  usage: number;
  destroyed: boolean;
  destroy(): void;
  /** What a readback maps: the bytes the fake GPU says the buffer holds. */
  contents?: Uint8Array;
  mapAsync(mode: number): Promise<void>;
  getMappedRange(): ArrayBuffer;
  unmap(): void;
}

export interface FakeTexture {
  label: string;
  format: string;
  size: number[];
  usage: number;
  destroyed: boolean;
  destroy(): void;
  createView(): { label: string };
}

/** One thing that reached the queue or a pass, in order. */
export type Event =
  | { kind: "writeBuffer"; buffer: string; offset: number; data: Uint8Array }
  | { kind: "submit" }
  | { kind: "dispatch"; pipeline: string; groups: number[]; offsets: number[]; bindGroup: FakeBindGroup }
  | { kind: "renderPass"; clear: GPUColor; view: string; depth?: string; load?: string }
  | { kind: "writeTexture"; texture: string; origin: unknown; bytesPerRow: number; size: unknown }
  | { kind: "copyBuffer"; from: string; fromOffset: number; to: string; toOffset: number; size: number }
  | { kind: "dispatchIndirect"; pipeline: string; buffer: string; offset: number }
  | { kind: "drawIndirect"; pipeline: string; buffer: string; offset: number }
  | { kind: "draw"; pipeline: string; vertices: number; instances: number; offsets: number[]; bindGroup: FakeBindGroup }
  | { kind: "copy"; from: string; to: string };

export interface FakeBindGroup {
  id: number;
  label: string;
  entries: { binding: number; buffer: string; offset: number | undefined; size: number | undefined }[];
  /** Each entry's buffer itself (buffers can share a label). */
  buffers: FakeBuffer[];
}

export class FakeDevice {
  readonly events: Event[] = [];
  /** Each compute and render pass's label, in the order they began. */
  readonly passLabels: string[] = [];
  readonly buffers: FakeBuffer[] = [];
  readonly textures: FakeTexture[] = [];
  readonly samplers: GPUSamplerDescriptor[] = [];
  /** Render pipelines made on demand (not at load), by label and target formats. */
  /** The render pipelines made at a draw (`createRenderPipeline`), and those made at load
   * (`createRenderPipelineAsync`): each `label colour|depth`. */
  readonly variants: string[] = [];
  readonly made: string[] = [];
  readonly layouts: GPUBindGroupLayoutDescriptor[] = [];
  /** Validation errors by label, which creating a bind group layout raises into the innermost
   * error scope. */
  readonly layoutErrors = new Map<string, string>();
  /** Errors by label, which creating a pipeline rejects with. */
  readonly pipelineErrors = new Map<string, string>();
  /** WebGPU's default limits, those the runtime reads. */
  readonly limits = {
    minUniformBufferOffsetAlignment: 256,
    minStorageBufferOffsetAlignment: 256,
    maxBufferSize: 268_435_456,
    maxStorageBufferBindingSize: 134_217_728,
    maxUniformBufferBindingSize: 65_536,
  };
  #bindGroups = 0;
  /** The error scope stack: each scope's first error. */
  readonly #scopes: (string | null)[] = [];

  queue = {
    writeBuffer: (buffer: FakeBuffer, offset: number, data: Uint8Array, dataOffset = 0, size?: number) => {
      const bytes = data.slice(dataOffset, size === undefined ? undefined : dataOffset + size);
      this.events.push({ kind: "writeBuffer", buffer: buffer.label, offset, data: bytes });
    },
    submit: () => {
      this.events.push({ kind: "submit" });
    },
    writeTexture: (dest: GPUTexelCopyTextureInfo, _data: Uint8Array, layout: GPUTexelCopyBufferLayout, size: GPUExtent3D) => {
      const texture = (dest.texture as unknown as FakeTexture).label;
      this.events.push({ kind: "writeTexture", texture, origin: dest.origin, bytesPerRow: layout.bytesPerRow!, size });
    },
  };

  createBuffer(desc: GPUBufferDescriptor): FakeBuffer {
    const buffer: FakeBuffer = {
      label: desc.label ?? "",
      size: desc.size,
      usage: desc.usage,
      destroyed: false,
      destroy() {
        this.destroyed = true;
      },
      async mapAsync() {},
      getMappedRange() {
        const out = new Uint8Array(this.size);
        out.set(this.contents ?? []);
        return out.buffer;
      },
      unmap() {},
    };
    this.buffers.push(buffer);
    return buffer;
  }

  createTexture(desc: GPUTextureDescriptor): FakeTexture {
    const label = desc.label ?? "";
    const texture = {
      label,
      format: desc.format,
      size: Array.from(desc.size as number[]),
      usage: desc.usage,
      destroyed: false,
      destroy() {
        this.destroyed = true;
      },
      createView: () => ({ label }),
    };
    this.textures.push(texture);
    return texture;
  }

  createSampler(desc: GPUSamplerDescriptor) {
    this.samplers.push(desc);
    return { label: desc.label };
  }

  createRenderPipeline(desc: GPURenderPipelineDescriptor) {
    const targets = Array.from(desc.fragment?.targets ?? []).map((t) => t?.format ?? "none");
    this.variants.push(`${desc.label} ${targets.join(",") || "none"}|${desc.depthStencil?.format ?? "none"}`);
    return { label: desc.label, kind: "render", desc };
  }

  createBindGroupLayout(desc: GPUBindGroupLayoutDescriptor) {
    this.layouts.push(desc);
    const error = this.layoutErrors.get(desc.label ?? "");
    const top = this.#scopes.length - 1;
    if (error !== undefined && top >= 0) this.#scopes[top] ??= error;
    return { label: desc.label };
  }

  createPipelineLayout(desc: GPUPipelineLayoutDescriptor) {
    return { label: desc.label };
  }

  createShaderModule(desc: GPUShaderModuleDescriptor) {
    const errorLine = desc.code.split("\n").findIndex((l) => l.includes("ERROR"));
    return {
      label: desc.label,
      getCompilationInfo: async () => ({
        messages:
          errorLine < 0 ? [] : [{ type: "error", lineNum: errorLine + 1, linePos: 3, message: "unresolved identifier" }],
      }),
    };
  }

  async createComputePipelineAsync(desc: GPUComputePipelineDescriptor) {
    this.#reject(desc.label);
    return { label: desc.label, kind: "compute" };
  }

  async createRenderPipelineAsync(desc: GPURenderPipelineDescriptor) {
    this.#reject(desc.label);
    const targets = Array.from(desc.fragment?.targets ?? []).map((t) => t?.format ?? "none");
    this.made.push(`${desc.label} ${targets.join(",") || "none"}|${desc.depthStencil?.format ?? "none"}`);
    return { label: desc.label, kind: "render", desc };
  }

  #reject(label = "") {
    const error = this.pipelineErrors.get(label);
    if (error !== undefined) throw new Error(error);
  }

  pushErrorScope(): void {
    this.#scopes.push(null);
  }

  /** Pops the scope when called, as WebGPU does; the promise gives its error. */
  popErrorScope(): Promise<{ message: string } | null> {
    if (this.#scopes.length === 0) return Promise.reject(new Error("no error scope to pop"));
    const message = this.#scopes.pop()!;
    return Promise.resolve(message === null ? null : { message });
  }

  createBindGroup(desc: GPUBindGroupDescriptor): FakeBindGroup {
    // A buffer's entry is `{ buffer, offset, size }`; a texture view's or sampler's is itself.
    const entries = Array.from(desc.entries, (e) => {
      const r = e.resource as GPUBufferBinding & { label?: string };
      return "buffer" in r ? { binding: e.binding, ...r } : { binding: e.binding, buffer: r as unknown as GPUBuffer };
    });
    const buffers = entries.map((e) => e.buffer as unknown as FakeBuffer);
    return {
      id: this.#bindGroups++,
      label: desc.label ?? "",
      entries: entries.map((e, i) => ({ binding: e.binding, buffer: buffers[i]!.label, offset: e.offset, size: e.size })),
      buffers,
    };
  }

  createCommandEncoder() {
    const events = this.events;
    const labels = this.passLabels;
    const pass = () => {
      let pipeline = "";
      let bindGroup: FakeBindGroup | null = null;
      let offsets: number[] = [];
      return {
        setPipeline(p: { label: string }) {
          pipeline = p.label;
        },
        setBindGroup(_: number, bg: FakeBindGroup, o: number[]) {
          bindGroup = bg;
          offsets = [...o];
        },
        dispatchWorkgroups(x: number, y: number, z: number) {
          events.push({ kind: "dispatch", pipeline, groups: [x, y, z], offsets, bindGroup: bindGroup! });
        },
        dispatchWorkgroupsIndirect(b: FakeBuffer, offset: number) {
          events.push({ kind: "dispatchIndirect", pipeline, buffer: b.label, offset });
        },
        draw(vertices: number, instances: number) {
          events.push({ kind: "draw", pipeline, vertices, instances, offsets, bindGroup: bindGroup! });
        },
        drawIndirect(b: FakeBuffer, offset: number) {
          events.push({ kind: "drawIndirect", pipeline, buffer: b.label, offset });
        },
        end() {},
      };
    };
    return {
      beginComputePass: (desc?: GPUComputePassDescriptor) => {
        labels.push(desc?.label ?? "");
        return pass();
      },
      beginRenderPass: (desc: GPURenderPassDescriptor) => {
        labels.push(desc.label ?? "");
        const [attachment] = Array.from(desc.colorAttachments);
        const view = (attachment?.view as unknown as { label: string } | undefined)?.label ?? "none";
        const event: Event = { kind: "renderPass", clear: attachment?.clearValue ?? { r: 0, g: 0, b: 0, a: 0 }, view };
        const depth = desc.depthStencilAttachment;
        if (depth !== undefined) event.depth = `${(depth.view as unknown as { label: string }).label} ${depth.depthLoadOp}`;
        if (attachment?.loadOp === "load") event.load = "load";
        events.push(event);
        return pass();
      },
      copyBufferToBuffer: (from: FakeBuffer, fromOffset: number, to: FakeBuffer, toOffset: number, size: number) => {
        events.push({ kind: "copyBuffer", from: from.label, fromOffset, to: to.label, toOffset, size });
      },
      copyTextureToTexture: (from: { texture: { label: string } }, to: { texture: { label: string } }) => {
        events.push({ kind: "copy", from: from.texture.label, to: to.texture.label });
      },
      finish: () => ({}),
    };
  }

  /** This fake as the type the runtime expects. */
  get gpu(): GPUDevice {
    return this as unknown as GPUDevice;
  }
}

/** A texture whose views are labelled with its name. */
export function fakeTexture(label: string): GPUTexture {
  return { label, createView: () => ({ label }) } as unknown as GPUTexture;
}
