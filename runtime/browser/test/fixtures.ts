// Shared test fixtures: the first-light build, and a manifest of known shapes.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Checker, type Limits } from "../src/check.ts";
import { type Manifest, validateManifest } from "../src/manifest.ts";
import type { Executor } from "../src/program.ts";
import type { Command } from "../src/stream.ts";

export const FIRST_LIGHT = join(import.meta.dir, "../../fixtures/first-light");

export const readFixture = (name: string) => new Uint8Array(readFileSync(join(FIRST_LIGHT, name)));

/** WebGPU's default limits, as both hosts use them. */
export const DEFAULT_LIMITS: Limits = { maxBufferSize: 134_217_728, maxWorkgroupsPerDimension: 65_535 };

/** Pipeline 0 renders and pipeline 1 computes; each takes 16 uniform bytes and binds a read-only
 * buffer then a read-write one (runtime/native/src/program/tests.rs uses the same shapes). */
export function shapes(): Manifest {
  const m: Manifest = {
    manifest_version: 1,
    stream_version: 2,
    wasm: "game.wasm",
    pipelines: [
      {
        kind: "render",
        name: "draw",
        shader: "draw.wgsl",
        vertex_entry: "vs",
        fragment_entry: "fs",
        uniform: { binding: 0, size: 16, space: "uniform" },
        buffers: [
          { binding: 1, access: "read" },
          { binding: 2, access: "read_write" },
        ],
      },
      {
        kind: "compute",
        name: "compute",
        shader: "compute.wgsl",
        entry: "main",
        workgroup_size: [64, 1, 1],
        uniform: { binding: 0, size: 16, space: "uniform" },
        buffers: [
          { binding: 1, access: "read" },
          { binding: 2, access: "read_write" },
        ],
      },
    ],
  };
  validateManifest(m);
  return m;
}

export const checker = (m: Manifest = shapes()) => new Checker(m, DEFAULT_LIMITS);

/** Records each command's opcode, and `end` at each frame's end. */
export class Recorder implements Executor {
  log: string[] = [];
  execute(cmd: Command): void {
    this.log.push(cmd.op);
  }
  flush(): void {
    this.log.push("end");
  }
}
