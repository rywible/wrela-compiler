// Loads a build (manifest.json, the WASM, each pipeline's WGSL) and assembles a running program:
// pipelines built up front, then the WASM instantiated against the decoder.

import { Checker } from "./check.ts";
import { deviceLimits } from "./device.ts";
import { errorMessage } from "./errors.ts";
import { buildPipelines, GpuExecutor, type PipelineCache, type ScreenTarget, type Timing } from "./gpu.ts";
import { Lines, LINES_SIDECAR } from "./lines.ts";
import { type Manifest, parseManifest } from "./manifest.ts";
import { pathProblem, pathUrl } from "./path.ts";
import { type Compiled, type Io, Program, type ProgramOptions } from "./program.ts";
import type { Bytes } from "./stream.ts";
import { UTF8 } from "./wasm.ts";
import { parseScript, type Scripted } from "./input.ts";

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
    // A build's files and a program's fetches stay at their checked URL. A server redirect
    // must not turn a relative path into a request to another origin.
    response = await fetchFn(url, { redirect: "manual" });
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

/** Loads a test's input script, with the native host's text and input rules. */
export async function loadScript(url: URL, fetchFn: Fetch = (url, init) => fetch(url, init)): Promise<Scripted[]> {
  return parseScript(await getText(fetchFn, url));
}

/**
 * The build's files that the page fetched ahead for the worker (`preloaded` in main.ts): the
 * page asks for them as soon as it is parsed, before its script, the worker's script and the
 * manifest have each come over the network in turn. A fetch of one takes its bytes, once; a
 * fetch of any other, or of one the page couldn't fetch, goes to the network.
 */
export class Handed {
  readonly #files = new Map<
    string,
    { bytes: Promise<Uint8Array<ArrayBuffer> | null>; give: (b: Uint8Array<ArrayBuffer> | null) => void; taken: boolean }
  >();

  constructor(urls: readonly string[]) {
    for (const url of urls) {
      let give: (b: Uint8Array<ArrayBuffer> | null) => void = () => {};
      const bytes = new Promise<Uint8Array<ArrayBuffer> | null>((resolve) => (give = resolve));
      this.#files.set(url, { bytes, give, taken: false });
    }
  }

  /** The page's fetch of `url` ended: its bytes, or null if it failed. */
  give(url: string, bytes: Uint8Array<ArrayBuffer> | null): void {
    this.#files.get(url)?.give(bytes);
  }

  /** `fetchFn`, but taking what the page fetched. */
  fetch(fetchFn: Fetch): Fetch {
    return async (url, init) => {
      const handed = this.#files.get(url.href);
      if (handed === undefined || handed.taken) return fetchFn(url, init);
      handed.taken = true;
      const bytes = await handed.bytes;
      return bytes === null ? fetchFn(url, init) : new Response(bytes);
    };
  }
}

/** Fetches and validates a build; its files resolve against `base`. */
export async function loadBuild(base: string, fetchFn: Fetch = (url, init) => fetch(url, init)): Promise<Build> {
  const manifest = parseManifest(await getText(fetchFn, new URL("manifest.json", base)));
  const [wasm, shaders] = await Promise.all([
    get(fetchFn, new URL(manifest.wasm, base)).then(async (r) => new Uint8Array(await r.arrayBuffer())),
    Promise.all(manifest.pipelines.map((p) => getText(fetchFn, new URL(p.shader, base)))),
  ]);
  return { manifest, wasm, shaders };
}

/** An optimized build's trap locations, from beside its WASM (`game.lines` for `game.wasm`);
 * null if it has none or they don't arrive. */
export async function loadLines(base: string, manifest: Manifest, fetchFn: Fetch = (url, init) => fetch(url, init)): Promise<Lines | null> {
  const name = manifest.wasm.replace(/\.wasm$/, "") + "." + LINES_SIDECAR;
  try {
    return Lines.decode(new Uint8Array(await (await get(fetchFn, new URL(name, base))).arrayBuffer()));
  } catch {
    return null;
  }
}

/**
 * Where a program in this browser keeps its requests' bytes: `Fetch` and `Post` use the build
 * at `files` (the page's build, `base`, unless a hot reload swapped another in), and storage is
 * the origin's private file system, under `wrela/` and the page's path (so two games on one
 * origin don't share it). Paths are checked before they get here (`pathProblem`).
 */
export function browserIo(
  base: string,
  fetchFn: Fetch = (url, init) => fetch(url, init),
  files: string = base,
  hostRequest?: (service: string, body: Bytes) => Promise<Bytes>,
  storage: string | null = null,
): Io {
  if (storage !== null && pathProblem(storage) !== undefined) throw new Error("invalid storage namespace");
  const parts = storage === null ? new URL(base).pathname.split("/").filter(p => p !== "") : storage.split("/");
  const root = ["wrela", ...parts];
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
    ...(hostRequest ? { hostRequest } : {}),
    async fetch(url) {
      const response = await get(fetchFn, pathUrl(files, url));
      return new Uint8Array(await response.arrayBuffer());
    },
    async storageRead(path) {
      try {
        const f = await (await file(path, false)).getFile();
        return new Uint8Array(await f.arrayBuffer());
      } catch (error) {
        // The private file system has nothing there: the file, or a directory on its way.
        if ((error as { name?: unknown } | null)?.name === "NotFoundError") return null;
        throw error;
      }
    },
    async storageWrite(path, data) {
      const w = await (await file(path, true)).createWritable();
      try {
        await w.write(data);
        await w.close();
      } catch (error) {
        // Release the writer even when its write or close failed, preserving that first error.
        await w.abort().catch(() => {});
        throw error;
      }
    },
    async post(url, body) {
      // Only to the build's own origin: the path is relative (`pathProblem`), and a redirect
      // must not send the body anywhere else.
      const response = await fetchFn(pathUrl(files, url), { method: "POST", body, redirect: "manual" });
      if (!response.ok) throw new Error(`POST ${url} failed: ${response.status} ${response.statusText}`);
      return new Uint8Array(await response.arrayBuffer());
    },
  };
}

/** How a program's GPU side is made. */
export interface GpuOptions {
  /** Whether passes and dispatches are timed, and how. */
  timing?: Timing;
  /** Called with how long building every pipeline took (ms). */
  onPipelines?: (ms: number) => void;
  /** Hot reload: the pipelines built so far, which the build keeps where they're the same
   * (`buildPipelines`). */
  cache?: PipelineCache | undefined;
}

/** Starts building every pipeline, compiles the WASM meanwhile (unless the build comes compiled:
 * one got ready for a hand-over), then instantiates the program with the decoder behind its
 * import. A program whose `init` fails leaves nothing on the GPU. The program starts while its pipelines build: a command
 * that needs one waits for it (`GpuExecutor`), and the frames wait for the commands
 * (`GpuExecutor.caughtUp`). A pipeline that can't be built fails the program, through the
 * executor's `built`. */
export async function startProgram(
  device: GPUDevice,
  build: Build & { compiled?: Compiled },
  screen: ScreenTarget,
  options: ProgramOptions = {},
  gpu: GpuOptions = {},
): Promise<Program> {
  const started = performance.now();
  const pipelines = buildPipelines(device, build.manifest, build.shaders, screen.format, gpu.cache);
  const compiled = build.compiled ?? (await Program.compile(build.wasm));
  const debug = build.manifest.pipelines.some((p) => p.debug_flag !== null);
  const executor = new GpuExecutor(device, pipelines, screen, gpu.timing, debug);
  executor.built.then(() => gpu.onPipelines?.(performance.now() - started), () => {});
  const checker = new Checker(build.manifest, deviceLimits(device));
  try {
    return await Program.instantiate(compiled, checker, executor, options);
  } catch (e) {
    executor.destroyAll();
    throw e;
  }
}
