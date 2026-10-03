// A fake WebGPU device that records what the runtime asks of it, for testing the decoder's
// mapping to WebGPU without a GPU. Only what the runtime uses is implemented.

export interface FakeBuffer {
  label: string;
  size: number;
  usage: number;
  destroyed: boolean;
  destroy(): void;
}

/** One thing that reached the queue or a pass, in order. */
export type Event =
  | { kind: "writeBuffer"; buffer: string; offset: number; data: Uint8Array }
  | { kind: "submit" }
  | { kind: "dispatch"; pipeline: string; groups: number[]; offsets: number[]; bindGroup: FakeBindGroup }
  | { kind: "renderPass"; clear: GPUColor; view: string }
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
  readonly buffers: FakeBuffer[] = [];
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
  };

  createBuffer(desc: GPUBufferDescriptor): FakeBuffer {
    const buffer = {
      label: desc.label ?? "",
      size: desc.size,
      usage: desc.usage,
      destroyed: false,
      destroy() {
        this.destroyed = true;
      },
    };
    this.buffers.push(buffer);
    return buffer;
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
    const entries = Array.from(desc.entries, (e) => ({ binding: e.binding, ...(e.resource as GPUBufferBinding) }));
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
        draw(vertices: number, instances: number) {
          events.push({ kind: "draw", pipeline, vertices, instances, offsets, bindGroup: bindGroup! });
        },
        end() {},
      };
    };
    return {
      beginComputePass: () => pass(),
      beginRenderPass: (desc: GPURenderPassDescriptor) => {
        const [attachment] = Array.from(desc.colorAttachments);
        const view = attachment!.view as unknown as { label: string };
        events.push({ kind: "renderPass", clear: attachment!.clearValue!, view: view.label });
        return pass();
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
