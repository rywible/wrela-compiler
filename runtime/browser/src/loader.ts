// Loads a build (manifest.json, the WASM, each pipeline's WGSL) and assembles a running program:
// pipelines built up front, then the WASM instantiated against the decoder.
//
// A build may ship `addresses.json` beside its manifest (language.md §15.2): the same-origin URLs,
// under `/b/`, that its payloads are served at, by their paths in the build. A payload is the
// manifest's WASM (either module), each pipeline's WGSL, their trap locations, and a file the
// program fetches; a payload the map doesn't name, and everything else (pages, `net.json`,
// icons), is where the build is. A map that names a URL outside `/b/` fails the load.

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
  /** Where its payloads are served, if it ships `addresses.json`. */
  addresses: Addresses | null;
  /** The WASM: its response, not yet read (it compiles as it streams in), or its bytes. Once
   * compiled, `compiled` holds the module and this isn't read again. */
  wasm: Response | Bytes;
  /** Each pipeline's WGSL, in manifest order. */
  shaders: string[];
  /** The WASM, compiled: set by the first start (`startProgram`) or a hand-over's getting ready,
   * so a build started again reuses it. */
  compiled?: Compiled;
}

export type Fetch = (url: URL, init?: RequestInit) => Promise<Response>;

/** The browser's own `fetch`, to the network. */
export const networkFetch: Fetch = (url, init) => fetch(url, init);

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

/** What `addresses.json` starts with: its format's name. */
export const ADDRESSES_FORMAT = "wrela-addresses-1";
/** Where a build's payloads may be served. */
export const ADDRESSES_PREFIX = "/b/";

/** A build's `addresses.json`: `{ "format": "wrela-addresses-1", "files": { <path>: <URL> } }`,
 * each path one in the build (`pathProblem`'s rules), each URL root-absolute under `/b/`, with no
 * query, fragment, `.` or `..` part, on the build's own origin. */
export class Addresses {
  readonly #files: Map<string, string>;

  private constructor(files: Map<string, string>) {
    this.#files = files;
  }

  /** The map in `text`; it throws what's wrong with it. */
  static parse(text: string): Addresses {
    let json: unknown;
    try {
      json = JSON.parse(text);
    } catch {
      throw new Error("addresses.json isn't JSON");
    }
    const o = json as { format?: unknown; files?: unknown };
    if (o === null || typeof o !== "object" || o.format !== ADDRESSES_FORMAT) throw new Error(`addresses.json's format isn't \`${ADDRESSES_FORMAT}\``);
    if (o.files === null || typeof o.files !== "object" || Array.isArray(o.files)) throw new Error("addresses.json's `files` isn't an object");
    const files = new Map<string, string>();
    for (const [path, url] of Object.entries(o.files as Record<string, unknown>)) {
      const why = pathProblem(path);
      if (why !== undefined) throw new Error(`addresses.json names \`${path}\`, which ${why}`);
      if (typeof url !== "string" || !addressed(url)) throw new Error(`addresses.json puts \`${path}\` outside ${ADDRESSES_PREFIX}: \`${String(url)}\``);
      files.set(path, url);
    }
    return new Addresses(files);
  }

  /** Where payload `path` (a checked path in the build at `base`) is: its address on the build's
   * origin, or beside the build. */
  url(base: string, path: string): URL {
    const at = this.#files.get(path);
    return at === undefined ? pathUrl(base, path) : new URL(at, new URL(base).origin);
  }
}

/** Whether `url` is an address a map may name: `/b/` and parts that are each a name. */
function addressed(url: string): boolean {
  if (!url.startsWith(ADDRESSES_PREFIX) || /[\\?#%]/.test(url)) return false;
  return url.slice(1).split("/").every((p) => p !== "" && p !== "." && p !== "..");
}

/** Where payload `path` of the build at `base` is, by its map if it has one. */
export const payloadUrl = (base: string, path: string, addresses: Addresses | null): URL => (addresses === null ? new URL(path, base) : addresses.url(base, path));

/** The build at `base`'s `addresses.json`, or null if it has none (a 404). */
async function loadAddresses(fetchFn: Fetch, base: string): Promise<Addresses | null> {
  const url = new URL("addresses.json", base);
  let response: Response;
  try {
    response = await fetchFn(url, { redirect: "manual" });
  } catch (e) {
    throw new Error(`can't fetch ${url}: ${errorMessage(e)}`);
  }
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`can't fetch ${url}: ${response.status} ${response.statusText}`);
  const bytes = await response.arrayBuffer();
  let text: string;
  try {
    text = UTF8.decode(bytes);
  } catch {
    throw new Error(`${url} isn't UTF-8`);
  }
  return Addresses.parse(text);
}

/** Loads a test's input script, with the native host's text and input rules. */
export async function loadScript(url: URL, fetchFn: Fetch = networkFetch): Promise<Scripted[]> {
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
      // Taken once: the worker keeps no copy of its bytes.
      this.#files.delete(url.href);
      return bytes === null ? fetchFn(url, init) : new Response(bytes);
    };
  }
}

/** A module of one function that does an `f32x4.relaxed_madd`: what a browser that runs relaxed
 * SIMD validates (Chrome and Bun do). */
export const RELAXED_PROBE = new Uint8Array([
  0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
  // A type: () -> v128.
  0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7b,
  0x03, 0x02, 0x01, 0x00,
  // Its body: three zero vectors, `f32x4.relaxed_madd` (0xfd 0x105), `end`.
  0x0a, 0x3d, 0x01, 0x3b, 0x00,
  ...[0, 1, 2].flatMap(() => [0xfd, 0x0c, ...new Array<number>(16).fill(0)]),
  0xfd, 0x85, 0x02, 0x0b,
]);

/** Whether this browser runs relaxed SIMD. */
export function runsRelaxedSimd(): boolean {
  try {
    return WebAssembly.validate(RELAXED_PROBE);
  } catch {
    return false;
  }
}

/** `manifest` as this browser loads it: a relaxed-SIMD build's relaxed module where the browser
 * runs relaxed SIMD (`relaxed`), its standard one elsewhere. */
export function chooseWasm(manifest: Manifest, relaxed: boolean = runsRelaxedSimd()): Manifest {
  if (manifest.wasm_relaxed === null || !relaxed) return manifest;
  return { ...manifest, wasm: manifest.wasm_relaxed, wasm_abi: manifest.wasm_relaxed_abi };
}

/** Fetches and validates a build; its files resolve against `base`, and its payloads by its
 * `addresses.json` if it ships one (fetched beside the manifest). */
export async function loadBuild(base: string, fetchFn: Fetch = networkFetch): Promise<Build> {
  // Both at once; the manifest's failure is the one told.
  const text = getText(fetchFn, new URL("manifest.json", base));
  const mapped = loadAddresses(fetchFn, base);
  mapped.catch(() => {});
  const manifest = chooseWasm(parseManifest(await text));
  const addresses = await mapped;
  // The WASM's response stays unread: `Program.compile` compiles it as it streams in.
  const [wasm, shaders] = await Promise.all([
    get(fetchFn, payloadUrl(base, manifest.wasm, addresses)),
    Promise.all(manifest.pipelines.map((p) => getText(fetchFn, payloadUrl(base, p.shader, addresses)))),
  ]);
  return { manifest, addresses, wasm, shaders };
}

/** An optimized build's trap locations, from beside its WASM (`game.lines` for `game.wasm`), or
 * where its map puts them; null if it has none or they don't arrive. */
export async function loadLines(base: string, manifest: Manifest, fetchFn: Fetch = networkFetch, addresses: Addresses | null = null): Promise<Lines | null> {
  const name = manifest.wasm.replace(/\.wasm$/, "") + "." + LINES_SIDECAR;
  try {
    return Lines.decode(new Uint8Array(await (await get(fetchFn, payloadUrl(base, name, addresses))).arrayBuffer()));
  } catch {
    return null;
  }
}

/**
 * Where a program in this browser keeps its requests' bytes: `Fetch` and `Post` use the build
 * at `files` (the page's build, `base`, unless a hot reload swapped another in), a `Fetch` by the
 * build's `addresses` where they name its file, and storage is the origin's private file system,
 * under `wrela/` and the page's path (so two games on one origin don't share it). Paths are
 * checked before they get here (`pathProblem`).
 */
export function browserIo(
  base: string,
  fetchFn: Fetch = networkFetch,
  files: string = base,
  hostRequest?: (service: string, body: Bytes) => Promise<Bytes>,
  storage: string | null = null,
  addresses: Addresses | null = null,
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
      const response = await get(fetchFn, addresses === null ? pathUrl(files, url) : addresses.url(files, url));
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
  /** Called with how long building every tier-0 pipeline took (ms): when the program's
   * unconditional commands can all run. */
  onPipelines?: (ms: number) => void;
  /** Called with how long building every pipeline, tier 1's too, took (ms). */
  onTier1?: (ms: number) => void;
  /** Hot reload: the pipelines built so far, which the build keeps where they're the same
   * (`buildPipelines`). */
  cache?: PipelineCache | undefined;
  /** Whether the WASM's bytes are hashed as it compiles (the tick log's header). */
  hashWasm?: boolean;
}

/** Gets every pipeline ready to build, the first frame's first (`buildPipelines`, held), compiles the WASM meanwhile (unless the build comes compiled:
 * one got ready for a hand-over, or one started before), then instantiates the program with the decoder behind its
 * import. A program whose `init` fails leaves nothing on the GPU. The program starts while its pipelines build: a command
 * that needs one waits for it (`GpuExecutor`), and the frames wait for the commands
 * (`GpuExecutor.caughtUp`). A pipeline that can't be built fails the program, through the
 * executor's `built`. */
export async function startProgram(
  device: GPUDevice,
  build: Build,
  screen: ScreenTarget,
  options: ProgramOptions = {},
  gpu: GpuOptions = {},
): Promise<Program> {
  const started = performance.now();
  // Held: the first frame's pipelines build first, alone (`GpuExecutor` releases the rest).
  const pipelines = buildPipelines(device, build.manifest, build.shaders, screen.format, gpu.cache, true);
  const compiled = (build.compiled ??= await Program.compile(build.wasm, build.manifest.wasm_abi, gpu.hashWasm ?? false));
  const debug = build.manifest.pipelines.some((p) => p.debug_flag !== null);
  const executor = new GpuExecutor(device, pipelines.each, screen, gpu.timing, debug, pipelines);
  executor.built.then(() => gpu.onPipelines?.(performance.now() - started), () => {});
  // Every pipeline, tier 1's too: how long the whole set took, timed after tier 0's (the tiers
  // build together, and the last of them may finish beside tier 0's last).
  executor.built.then(() => Promise.all(pipelines.each)).then(() => gpu.onTier1?.(performance.now() - started), () => {});
  const checker = new Checker(build.manifest, deviceLimits(device));
  try {
    const instantiating = performance.now();
    const program = await Program.instantiate(compiled, checker, executor, options);
    // Its start function and `init`, where a lazily compiled module compiles what they call.
    compiled.instantiate_ms ??= performance.now() - instantiating;
    return program;
  } catch (e) {
    executor.destroyAll();
    throw e;
  }
}
