// Loads a build (manifest.json, the WASM, each pipeline's WGSL) and assembles a running program:
// pipelines built up front, then the WASM instantiated against the decoder.

import { Checker, limitsOf } from "./check.ts";
import { buildPipelines, GpuExecutor, type ScreenTarget } from "./gpu.ts";
import { type Manifest, parseManifest } from "./manifest.ts";
import { Program } from "./program.ts";

export interface Build {
  manifest: Manifest;
  wasm: Uint8Array<ArrayBuffer>;
  /** Each pipeline's WGSL, in manifest order. */
  shaders: string[];
}

export type Fetch = (url: URL) => Promise<Response>;

async function get(fetchFn: Fetch, url: URL): Promise<Response> {
  let response: Response;
  try {
    response = await fetchFn(url);
  } catch (e) {
    throw new Error(`can't fetch ${url}: ${e instanceof Error ? e.message : String(e)}`);
  }
  if (!response.ok) throw new Error(`can't fetch ${url}: ${response.status} ${response.statusText}`);
  return response;
}

/** Fetches and validates a build; its files resolve against `base`. */
export async function loadBuild(base: string, fetchFn: Fetch = (url) => fetch(url)): Promise<Build> {
  const manifest = parseManifest(await (await get(fetchFn, new URL("manifest.json", base))).text());
  const [wasm, shaders] = await Promise.all([
    get(fetchFn, new URL(manifest.wasm, base)).then(async (r) => new Uint8Array(await r.arrayBuffer())),
    Promise.all(manifest.pipelines.map(async (p) => (await get(fetchFn, new URL(p.shader, base))).text())),
  ]);
  return { manifest, wasm, shaders };
}

export interface Host {
  program: Program;
  executor: GpuExecutor;
}

/** Builds every pipeline, then instantiates the program with the decoder behind its import. */
export async function startProgram(device: GPUDevice, build: Build, screen: ScreenTarget): Promise<Host> {
  const pipelines = await buildPipelines(device, build.manifest, build.shaders);
  const executor = new GpuExecutor(device, pipelines, screen);
  const checker = new Checker(build.manifest, limitsOf(device.limits));
  const program = await Program.load(build.wasm, checker, executor);
  return { program, executor };
}
