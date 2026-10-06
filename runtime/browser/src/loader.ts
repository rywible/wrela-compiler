// Loads a build (manifest.json, the WASM, each pipeline's WGSL) and assembles a running program:
// pipelines built up front, then the WASM instantiated against the decoder.

import { Checker, limitsOf } from "./check.ts";
import { errorMessage } from "./errors.ts";
import { buildPipelines, GpuExecutor, type ScreenTarget } from "./gpu.ts";
import { type Manifest, parseManifest } from "./manifest.ts";
import { type Io, Program, type ProgramOptions } from "./program.ts";
import type { Bytes } from "./stream.ts";
import { UTF8 } from "./wasm.ts";

export interface Build {
  manifest: Manifest;
  wasm: Bytes;
  /** Each pipeline's WGSL, in manifest order. */
  shaders: string[];
}

export type Fetch = (url: URL, init?: RequestInit) => Promise<Response>;

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

/**
 * Where a program in this browser keeps its requests' bytes: `Fetch` reads a file of the build
 * at `base`, and storage is the origin's private file system, under `wrela/` and the build's
 * path (so two games on one origin don't share it). Paths are checked before they get here
 * (`pathProblem`).
 */
export function browserIo(base: string, fetchFn: Fetch = (url, init) => fetch(url, init)): Io {
  const root = ["wrela", ...new URL(base).pathname.split("/").filter((p) => p !== "")];
  const dir = async (parts: string[], create: boolean) => {
    let d = await navigator.storage.getDirectory();
    for (const p of parts) d = await d.getDirectoryHandle(p, { create });
    return d;
  };
  const file = async (path: string, create: boolean) => {
    const parts = [...root, ...path.split("/")];
    const name = parts.pop()!;
    return (await dir(parts, create)).getFileHandle(name, { create });
  };
  return {
    async fetch(url) {
      const response = await get(fetchFn, new URL(url, base));
      return new Uint8Array(await response.arrayBuffer());
    },
    async storageRead(path) {
      const f = await (await file(path, false)).getFile();
      return new Uint8Array(await f.arrayBuffer());
    },
    async storageWrite(path, data) {
      const w = await (await file(path, true)).createWritable();
      await w.write(data);
      await w.close();
    },
    async post(url, body) {
      // Only to the build's own origin: the path is relative (`pathProblem`).
      const response = await fetchFn(new URL(url, base), { method: "POST", body });
      if (!response.ok) throw new Error(`POST ${url} failed: ${response.status} ${response.statusText}`);
      return new Uint8Array(await response.arrayBuffer());
    },
  };
}

/** Builds every pipeline while the WASM compiles, then instantiates the program with the
 * decoder behind its import. */
export async function startProgram(
  device: GPUDevice,
  build: Build,
  screen: ScreenTarget,
  options: ProgramOptions = {},
  timestamps = false,
  onPipelines?: (ms: number) => void,
): Promise<Program> {
  const started = performance.now();
  const built = buildPipelines(device, build.manifest, build.shaders).then((p) => {
    onPipelines?.(performance.now() - started);
    return p;
  });
  const [pipelines, compiled] = await Promise.all([built, Program.compile(build.wasm)]);
  const executor = new GpuExecutor(device, pipelines, screen, timestamps);
  const checker = new Checker(build.manifest, limitsOf(device.limits));
    return Program.instantiate(compiled, checker, executor, options);
}
