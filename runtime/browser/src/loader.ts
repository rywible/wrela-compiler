// Loads a build (manifest.json, the WASM, each pipeline's WGSL) and assembles a running program:
// pipelines built up front, then the WASM instantiated against the decoder.

import { Checker } from "./check.ts";
import { errorMessage } from "./errors.ts";
import { buildPipelines, GpuExecutor, type ScreenTarget } from "./gpu.ts";
import { type Manifest, parseManifest } from "./manifest.ts";
import { Program, type ProgramOptions } from "./program.ts";
import type { Bytes } from "./stream.ts";
import { UTF8 } from "./wasm.ts";

export interface Build {
  manifest: Manifest;
  wasm: Bytes;
  /** Each pipeline's WGSL, in manifest order. */
  shaders: string[];
}

export type Fetch = (url: URL) => Promise<Response>;

async function get(fetchFn: Fetch, url: URL): Promise<Response> {
  let response: Response;
  try {
    response = await fetchFn(url);
  } catch (e) {
    throw new Error(`can't fetch ${url}: ${errorMessage(e)}`);
  }
  if (!response.ok) throw new Error(`can't fetch ${url}: ${response.status} ${response.statusText}`);
  return response;
}

/** A text file, read as the native host reads one: strict UTF-8, any byte-order mark kept
 * (`Response.text()` would drop the mark and replace bad bytes). */
async function getText(fetchFn: Fetch, url: URL): Promise<string> {
  const bytes = await (await get(fetchFn, url)).arrayBuffer();
  try {
    return UTF8.decode(bytes);
  } catch {
    throw new Error(`${url} isn't UTF-8`);
  }
}

/** Fetches and validates a build; its files resolve against `base`. */
export async function loadBuild(base: string, fetchFn: Fetch = (url) => fetch(url)): Promise<Build> {
  const manifest = parseManifest(await getText(fetchFn, new URL("manifest.json", base)));
  const [wasm, shaders] = await Promise.all([
    get(fetchFn, new URL(manifest.wasm, base)).then(async (r) => new Uint8Array(await r.arrayBuffer())),
    Promise.all(manifest.pipelines.map((p) => getText(fetchFn, new URL(p.shader, base)))),
  ]);
  return { manifest, wasm, shaders };
}

/** Builds every pipeline while the WASM compiles, then instantiates the program with the
 * decoder behind its import. */
export async function startProgram(
  device: GPUDevice,
  build: Build,
  screen: ScreenTarget,
  options: ProgramOptions = {},
): Promise<Program> {
  const [pipelines, module] = await Promise.all([
    buildPipelines(device, build.manifest, build.shaders),
    Program.compile(build.wasm),
  ]);
  const executor = new GpuExecutor(device, pipelines, screen);
  const checker = new Checker(build.manifest);
  return Program.instantiate(module, checker, executor, options);
}
