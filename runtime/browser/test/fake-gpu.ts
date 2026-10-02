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
}

export const DEFAULT_GPU_LIMITS = {
  minUniformBufferOffsetAlignment: 256,
  minStorageBufferOffsetAlignment: 256,
  maxBufferSize: 268_435_456,
  maxStorageBufferBindingSize: 134_217_728,
  maxUniformBufferBindingSize: 65_536,
  maxComputeWorkgroupsPerDimension: 65_535,
  maxTextureDimension2D: 8192,
};

export class FakeDevice {
  readonly events: Event[] = [];
  readonly buffers: FakeBuffer[] = [];
  readonly layouts: GPUBindGroupLayoutDescriptor[] = [];
  /** The validation error the next popErrorScope reports, if any. */
  scopeError: string | null = null;
  limits = DEFAULT_GPU_LIMITS;
  #bindGroups = 0;

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
    return { label: desc.label, kind: "compute" };
  }

  async createRenderPipelineAsync(desc: GPURenderPipelineDescriptor) {
    return { label: desc.label, kind: "render", desc };
  }

  pushErrorScope(): void {}

  async popErrorScope() {
    const message = this.scopeError;
    this.scopeError = null;
    return message === null ? null : { message };
  }

  createBindGroup(desc: GPUBindGroupDescriptor): FakeBindGroup {
    return {
      id: this.#bindGroups++,
      label: desc.label ?? "",
      entries: Array.from(desc.entries, (e) => {
        const r = e.resource as GPUBufferBinding;
        return { binding: e.binding, buffer: (r.buffer as unknown as FakeBuffer).label, offset: r.offset, size: r.size };
      }),
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
