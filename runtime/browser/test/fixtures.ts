// Shared test fixtures: the first-light build, the ABI's vectors, and a manifest of known shapes.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { MANIFEST_VERSION, STREAM_VERSION } from "../src/abi.gen.ts";
import { Checker } from "../src/check.ts";
import { type Manifest, validateManifest } from "../src/manifest.ts";
import type { Executor } from "../src/program.ts";
import type { Command } from "../src/stream.ts";

export const FIRST_LIGHT = join(import.meta.dir, "../../fixtures/first-light");

export const readFixture = (name: string) => new Uint8Array(readFileSync(join(FIRST_LIGHT, name)));
export const readFixtureText = (name: string) => readFileSync(join(FIRST_LIGHT, name), "utf8");

export interface ErrorVector {
  kind: string;
  message: string;
}

export interface Vectors {
  hashes: { bytes: string; hash: string }[];
  batches: { name: string; bytes: string; outcome: { commands?: unknown[]; error?: ErrorVector } }[];
  sequences: { name: string; bytes: string; error: ErrorVector | null }[];
  checks: { manifest: string; batches: { name: string; bytes: string; error: { opcode: string; message: string } | null }[] };
  /** `malformed`: JSON that doesn't parse, which each host rejects in its own words. */
  manifests: { name: string; json: string; error: string | null; malformed?: boolean }[];
  lines: { name: string; bytes: string; at: [number, string | null][]; error: string | null }[];
  input: { name: string; script: string; at?: string[]; events?: string[]; error?: string }[];
  tick_logs: { wasm_hash: string; hz: number; first: string; ticks: { records: string[]; hash: string }[]; bytes: string }[];
  lockstep: { hz: number; fps: number; frames: number[]; ticks: number[]; times: number[] }[];
}

/** The ABI's test vectors (runtime/abi/vectors.json). */
export const vectors: Vectors = JSON.parse(readFileSync(join(import.meta.dir, "../../abi/vectors.json"), "utf8"));

/** Pipeline 0 renders and pipeline 1 computes; each takes 16 uniform bytes and binds a read-only
 * buffer then a read-write one (runtime/native/src/program/tests.rs uses the same shapes). */
export function shapes(): Manifest {
  const m: Manifest = {
    manifest_version: MANIFEST_VERSION,
    stream_version: STREAM_VERSION,
    wasm: "game.wasm",
    pipelines: [
      {
        kind: "render",
        name: "draw",
        shader: "draw.wgsl",
        vertex_entry: "vs",
        fragment_entry: "fs",
        blend: false,
        cull: "none",
        depth_bias: { constant: 0, slope_scale: 0, clamp: 0 },
        uniform: { binding: 0, size: 16, space: "uniform" },
        bindings: [
          { binding: 1, kind: "read" },
          { binding: 2, kind: "read_write" },
        ],
        debug_flag: null,
      },
      {
        kind: "compute",
        name: "compute",
        shader: "compute.wgsl",
        entry: "main",
        workgroup_size: [64, 1, 1],
        uniform: { binding: 0, size: 16, space: "uniform" },
        bindings: [
          { binding: 1, kind: "read" },
          { binding: 2, kind: "read_write" },
        ],
        debug_flag: null,
      },
    ],
  };
  validateManifest(m);
  return m;
}

export const checker = (m: Manifest = shapes()) => new Checker(m);

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
