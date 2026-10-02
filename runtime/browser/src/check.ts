// Checks each command against the manifest and the program's buffers before it runs. Mirrors
// runtime/native/src/check.rs (same rules, same messages), so both hosts reject the same
// programs at the command that's wrong, rather than with an asynchronous WebGPU error that names
// no command.

import type { Access, Manifest } from "./manifest.ts";
import type { Command, OpcodeName } from "./stream.ts";

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

export interface Limits {
  /** The lesser of `maxBufferSize` and `maxStorageBufferBindingSize`. */
  maxBufferSize: number;
  maxWorkgroupsPerDimension: number;
}

/** The limits of a device requested without `requiredLimits`, as both hosts request it. */
export function limitsOf(limits: GPUSupportedLimits): Limits {
  return {
    maxBufferSize: Math.min(limits.maxBufferSize, limits.maxStorageBufferBindingSize),
    maxWorkgroupsPerDimension: limits.maxComputeWorkgroupsPerDimension,
  };
}

interface Shape {
  name: string;
  compute: boolean;
  uniformSize: number;
  buffers: Access[];
}

/** Each bound buffer's access so far in a usage scope. */
type Usage = Map<number, { read: boolean; write: boolean }>;

export class Checker {
  readonly #pipelines: Shape[];
  readonly #buffers = new Map<number, number>();
  #pass: Usage | null = null;

  constructor(
    manifest: Manifest,
    readonly limits: Limits,
  ) {
    this.#pipelines = manifest.pipelines.map((p) => ({
      name: p.name,
      compute: p.kind === "compute",
      uniformSize: p.uniform?.size ?? 0,
      buffers: p.buffers.map((b) => b.access),
    }));
  }

  /** Checks a command that has passed the `Sequencer`, and records its effect. */
  check(cmd: Command): void {
    const err = (why: string) => new CommandError(cmd.op, why);
    switch (cmd.op) {
      case "CreateBuffer": {
        const { handle, size } = cmd;
        if (this.#buffers.has(handle)) throw err(`buffer ${handle} already exists`);
        const limit = this.limits.maxBufferSize;
        if (size > limit) throw err(`buffer ${handle} is ${size} bytes; the limit is ${limit}`);
        this.#buffers.set(handle, size);
        return;
      }
      case "WriteBuffer": {
        const { handle, offset, data } = cmd;
        const size = this.#size(cmd.op, handle);
        if (offset + data.length > size) {
          throw err(`writing ${data.length} bytes at offset ${offset} overruns buffer ${handle} (${size} bytes)`);
        }
        return;
      }
      case "Dispatch": {
        this.#binding(cmd.op, cmd.pipeline, cmd.buffers, cmd.uniforms.length, new Map());
        const max = this.limits.maxWorkgroupsPerDimension;
        if (cmd.groups.some((g) => g > max)) {
          throw err(`${cmd.groups.join("x")} workgroups is over the limit of ${max} per dimension`);
        }
        return;
      }
      case "BeginScreenPass":
        this.#pass = new Map();
        return;
      case "Draw":
        this.#pass ??= new Map();
        this.#binding(cmd.op, cmd.pipeline, cmd.buffers, cmd.uniforms.length, this.#pass);
        return;
      case "Present":
        this.#pass = null;
        return;
    }
  }

  #size(op: OpcodeName, handle: number): number {
    const size = this.#buffers.get(handle);
    if (size === undefined) throw new CommandError(op, `there's no buffer ${handle}`);
    return size;
  }

  #binding(op: OpcodeName, pipeline: number, buffers: number[], uniformLen: number, usage: Usage): void {
    const err = (why: string) => new CommandError(op, why);
    const p = this.#pipelines[pipeline];
    if (p === undefined) {
      throw err(`there's no pipeline ${pipeline} (the manifest has ${this.#pipelines.length})`);
    }
    const name = p.name;
    if (p.compute && op === "Draw") {
      throw err(`pipeline ${pipeline} (${name}) is a compute pipeline; Draw needs a render pipeline`);
    }
    if (!p.compute && op === "Dispatch") {
      throw err(`pipeline ${pipeline} (${name}) is a render pipeline; Dispatch needs a compute pipeline`);
    }
    if (buffers.length !== p.buffers.length) {
      throw err(
        `pipeline ${pipeline} (${name}) binds ${p.buffers.length} buffers, but the command lists ${buffers.length}`,
      );
    }
    if (uniformLen !== p.uniformSize) {
      throw err(
        `pipeline ${pipeline} (${name}) takes ${p.uniformSize} uniform bytes, but the command has ${uniformLen}`,
      );
    }
    const scope = op === "Dispatch" ? "dispatch" : "screen pass";
    buffers.forEach((handle, i) => {
      this.#size(op, handle);
      const seen = usage.get(handle) ?? { read: false, write: false };
      if (p.buffers[i] === "read") seen.read = true;
      else seen.write = true;
      usage.set(handle, seen);
      if (seen.read && seen.write) {
        throw err(`buffer ${handle} is bound both read-only and read-write in one ${scope}`);
      }
    });
  }
}
