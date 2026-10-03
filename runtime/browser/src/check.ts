// Checks each command against the manifest and the program's buffers before it runs. Mirrors
// runtime/abi/src/check.rs (same rules, same messages, checked by the ABI's test vectors), so
// both hosts reject the same programs at the command that's wrong, rather than with an
// asynchronous WebGPU error that names no command.

import { MAX_BUFFER_SIZE, MAX_WORKGROUPS_PER_DIMENSION } from "./abi.gen.ts";
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

interface Shape {
  name: string;
  compute: boolean;
  uniformSize: number;
  buffers: Access[];
}

/** Each bound buffer's access so far in a usage scope: `READ`, `WRITE` or both. */
type Usage = Map<number, number>;
const READ = 1;
const WRITE = 2;

export class Checker {
  readonly #pipelines: Shape[];
  readonly #buffers = new Map<number, number>();
  /** The current usage scope, a dispatch or the open screen pass. Dispatches happen only
   * outside a screen pass (the `Sequencer` checks), so the two kinds never overlap and share it. */
  readonly #scope: Usage = new Map();

  constructor(manifest: Manifest) {
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
        if (size > MAX_BUFFER_SIZE) throw err(`buffer ${handle} is ${size} bytes; the limit is ${MAX_BUFFER_SIZE}`);
        this.#buffers.set(handle, size);
        return;
      }
      case "DestroyBuffer":
        this.#size(cmd.op, cmd.handle);
        this.#buffers.delete(cmd.handle);
        return;
      case "WriteBuffer": {
        const { handle, offset, data } = cmd;
        const size = this.#size(cmd.op, handle);
        if (offset + data.length > size) {
          throw err(`writing ${data.length} bytes at offset ${offset} overruns buffer ${handle} (${size} bytes)`);
        }
        return;
      }
      case "Dispatch": {
        this.#scope.clear();
        this.#binding(cmd.op, cmd.pipeline, cmd.buffers, cmd.uniforms.length);
        const max = MAX_WORKGROUPS_PER_DIMENSION;
        if (cmd.groups.some((g) => g > max)) {
          throw err(`${cmd.groups.join("x")} workgroups is over the limit of ${max} per dimension`);
        }
        return;
      }
      case "BeginScreenPass": {
        const i = cmd.clear.findIndex((c) => !Number.isFinite(c));
        if (i >= 0) throw err(`the clear colour's ${"rgba"[i]} isn't a finite number`);
        this.#scope.clear();
        return;
      }
      case "Draw":
        this.#binding(cmd.op, cmd.pipeline, cmd.buffers, cmd.uniforms.length);
        return;
      case "Present":
        return;
    }
  }

  #size(op: OpcodeName, handle: number): number {
    const size = this.#buffers.get(handle);
    if (size === undefined) throw new CommandError(op, `there's no buffer ${handle}`);
    return size;
  }

  #binding(op: OpcodeName, pipeline: number, buffers: number[], uniformLen: number): void {
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
    const one = op === "Dispatch" ? "dispatch" : "draw";
    const written = new Set<number>();
    buffers.forEach((handle, i) => {
      this.#size(op, handle);
      if (p.buffers[i] === "read_write") {
        if (written.has(handle)) throw err(`buffer ${handle} is bound read-write twice in one ${one}`);
        written.add(handle);
      }
      const seen = (this.#scope.get(handle) ?? 0) | (p.buffers[i] === "read" ? READ : WRITE);
      this.#scope.set(handle, seen);
      if (seen === (READ | WRITE)) {
        throw err(`buffer ${handle} is bound both read-only and read-write in one ${scope}`);
      }
    });
  }
}
