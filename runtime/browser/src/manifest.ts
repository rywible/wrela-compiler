// The manifest, version 1: parsing and validation. Mirrors `Manifest::parse` and
// `Manifest::validate` in runtime/abi/src/manifest.rs: the same version rules, the same reading
// of each field, the same checks in the same order, the same messages. (Only JSON that doesn't
// parse is reported in each host's own words.)

import {
  MANIFEST_VERSION,
  MAX_STORAGE_BUFFERS_PER_STAGE,
  MAX_UNIFORM_BUFFER_BINDING_SIZE,
  MAX_WORKGROUP_INVOCATIONS,
  MAX_WORKGROUP_SIZE,
  STREAM_VERSION,
} from "./abi.gen.ts";
import { errorMessage } from "./errors.ts";

export type UniformSpace = "uniform" | "storage";
export type Access = "read" | "read_write";

export interface UniformBlock {
  binding: number;
  /** Bytes; every dispatch or draw of the pipeline carries exactly this many uniform bytes. */
  size: number;
  space: UniformSpace;
}

export interface BufferBinding {
  binding: number;
  access: Access;
}

export type Stage =
  | { kind: "compute"; entry: string; workgroup_size: [number, number, number] }
  | { kind: "render"; vertex_entry: string; fragment_entry: string };

export type Pipeline = Stage & {
  name: string;
  shader: string;
  uniform: UniformBlock | null;
  buffers: BufferBinding[];
};

export interface Manifest {
  manifest_version: number;
  stream_version: number;
  wasm: string;
  pipelines: Pipeline[];
}

export class ManifestError extends Error {
  constructor(why: string) {
    super(`invalid manifest: ${why}`);
    this.name = "ManifestError";
  }
}

type Json = Record<string, unknown>;

/** In a `u` regex a surrogate pair is one code point, so this finds only unpaired ones. */
const LONE_SURROGATE = /\p{Cs}/u;

const isObject = (v: unknown): v is Json => typeof v === "object" && v !== null && !Array.isArray(v);

/** A JSON value as both hosts show it: a scalar as JavaScript writes it, else its kind. */
function show(v: unknown): string {
  if (Array.isArray(v)) return "an array";
  if (isObject(v)) return "an object";
  return typeof v === "string" ? JSON.stringify(v) : String(v);
}

function field(o: Json, key: string, where: string): unknown {
  if (!(key in o)) throw new ManifestError(`missing field \`${key}\` in ${where}`);
  return o[key];
}

function asU32(v: unknown, what: string): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0 || v > 0xffff_ffff) {
    throw new ManifestError(`${what} must be a u32, not ${show(v)}`);
  }
  return v;
}

const u32 = (o: Json, key: string, where: string) => asU32(field(o, key, where), `${where}.${key}`);

function str(o: Json, key: string, where: string): string {
  const v = field(o, key, where);
  if (typeof v !== "string") throw new ManifestError(`${where}.${key} must be a string`);
  return v;
}

function object(v: unknown, where: string): Json {
  if (!isObject(v)) throw new ManifestError(`${where} must be an object`);
  return v;
}

function oneOf<T extends string>(o: Json, key: string, where: string, options: readonly T[]): T {
  const v = field(o, key, where);
  if (!options.includes(v as T)) {
    throw new ManifestError(`${where}.${key} must be one of ${options.join(", ")}, not ${show(v)}`);
  }
  return v as T;
}

function pipeline(v: unknown, i: number): Pipeline {
  const where = `pipelines[${i}]`;
  const o = object(v, where);
  const name = str(o, "name", where);
  const shader = str(o, "shader", where);
  // A missing `uniform` is read as null.
  const u = o["uniform"] ?? null;
  let uniform: UniformBlock | null = null;
  if (u !== null) {
    const uo = object(u, `${where}.uniform`);
    uniform = {
      binding: u32(uo, "binding", `${where}.uniform`),
      size: u32(uo, "size", `${where}.uniform`),
      space: oneOf(uo, "space", `${where}.uniform`, ["uniform", "storage"] as const),
    };
  }
  const bufs = field(o, "buffers", where);
  if (!Array.isArray(bufs)) throw new ManifestError(`${where}.buffers must be an array`);
  const buffers = bufs.map((b, j) => {
    const bw = `${where}.buffers[${j}]`;
    const bo = object(b, bw);
    return { binding: u32(bo, "binding", bw), access: oneOf(bo, "access", bw, ["read", "read_write"] as const) };
  });
  const kind = oneOf(o, "kind", where, ["compute", "render"] as const);
  if (kind === "compute") {
    const ws = field(o, "workgroup_size", where);
    if (!Array.isArray(ws) || ws.length !== 3) {
      throw new ManifestError(`${where}.workgroup_size must be an array of 3 u32s`);
    }
    const sizes = ws.map((s: unknown, k) => asU32(s, `${where}.workgroup_size[${k}]`));
    return {
      kind,
      name,
      shader,
      entry: str(o, "entry", where),
      workgroup_size: [sizes[0]!, sizes[1]!, sizes[2]!],
      uniform,
      buffers,
    };
  }
  return {
    kind,
    name,
    shader,
    vertex_entry: str(o, "vertex_entry", where),
    fragment_entry: str(o, "fragment_entry", where),
    uniform,
    buffers,
  };
}

/** How deep serde_json, and so the native host, nests arrays and objects. */
const MAX_NESTING = 127;

/** How deep arrays and objects nest in a parsed JSON value. */
function nesting(v: unknown): number {
  let deepest = 0;
  const stack: [unknown, number][] = [[v, 0]];
  while (stack.length > 0) {
    const [x, d] = stack.pop()!;
    if (x === null || typeof x !== "object") continue;
    deepest = Math.max(deepest, d + 1);
    for (const y of Object.values(x)) stack.push([y, d + 1]);
  }
  return deepest;
}

/** Parses and validates a manifest; any other version is rejected. */
export function parseManifest(json: string): Manifest {
  let v: unknown;
  try {
    // serde_json, and so the native host, rejects a lone UTF-16 surrogate anywhere.
    v = JSON.parse(json, (key, value: unknown) => {
      if (LONE_SURROGATE.test(key) || (typeof value === "string" && LONE_SURROGATE.test(value))) {
        throw new Error("a string has a lone surrogate");
      }
      // serde_json rejects a number past f64's range (`1e400`); JSON.parse makes it infinite.
      if (typeof value === "number" && !Number.isFinite(value)) {
        throw new Error("a number is out of range");
      }
      return value;
    });
    // serde_json reads arrays and objects nested at most 127 deep.
    if (nesting(v) > MAX_NESTING) throw new Error(`arrays and objects nest more than ${MAX_NESTING} deep`);
  } catch (e) {
    throw new ManifestError(errorMessage(e));
  }
  const version = isObject(v) ? v["manifest_version"] : undefined;
  if (version === undefined) {
    throw new ManifestError(`no manifest_version; this host reads version ${MANIFEST_VERSION}`);
  }
  if (version !== MANIFEST_VERSION) {
    throw new ManifestError(
      `manifest version ${show(version)}, but this host reads version ${MANIFEST_VERSION}`,
    );
  }
  const o = v as Json;
  const pipelines = field(o, "pipelines", "the manifest");
  if (!Array.isArray(pipelines)) throw new ManifestError("pipelines must be an array");
  const m: Manifest = {
    manifest_version: u32(o, "manifest_version", "the manifest"),
    stream_version: u32(o, "stream_version", "the manifest"),
    wasm: str(o, "wasm", "the manifest"),
    pipelines: pipelines.map(pipeline),
  };
  validateManifest(m);
  return m;
}

/** Whether a file the manifest names is one in the build directory, as both hosts read it (the
 * ABI's `plain_file_name`): letters, digits, `_`, `-` and `.`, not starting with `.`. */
export function plainFileName(name: string): boolean {
  return /^[A-Za-z0-9_.-]+$/.test(name) && !name.startsWith(".");
}

export function validateManifest(m: Manifest): void {
  const err = (why: string) => new ManifestError(why);
  if (m.manifest_version !== MANIFEST_VERSION) {
    throw err(`manifest version ${m.manifest_version}, expected ${MANIFEST_VERSION}`);
  }
  if (m.stream_version !== STREAM_VERSION) {
    throw err(`command stream version ${m.stream_version}, but this host reads ${STREAM_VERSION}`);
  }
  if (m.wasm === "") throw err("no WASM file");
  if (!plainFileName(m.wasm)) throw err("the WASM file's name isn't a file name in the build directory");
  m.pipelines.forEach((p, i) => {
    if (p.shader === "") throw err(`pipeline ${i} has no shader`);
    if (!plainFileName(p.shader)) {
      throw err(`pipeline ${i}'s shader name isn't a file name in the build directory`);
    }
    const bindings = p.buffers.map((b) => b.binding);
    const u = p.uniform;
    if (u !== null) {
      if (u.size === 0 || u.size % 4 !== 0) {
        throw err(`pipeline ${i}'s uniform size ${u.size} isn't a positive multiple of 4`);
      }
      if (u.space === "uniform" && u.size % 16 !== 0) {
        throw err(`pipeline ${i}'s uniform block of ${u.size} bytes isn't a multiple of 16`);
      }
      if (u.space === "uniform" && u.size > MAX_UNIFORM_BUFFER_BINDING_SIZE) {
        throw err(
          `pipeline ${i}'s uniform block of ${u.size} bytes is over WebGPU's default limit of ${MAX_UNIFORM_BUFFER_BINDING_SIZE}`,
        );
      }
      bindings.push(u.binding);
    }
    if (new Set(bindings).size !== bindings.length) throw err(`pipeline ${i} uses a binding twice`);
    const storage = p.buffers.length + (u?.space === "storage" ? 1 : 0);
    if (storage > MAX_STORAGE_BUFFERS_PER_STAGE) {
      throw err(
        `pipeline ${i} has ${storage} storage buffers; WebGPU's default limit is ${MAX_STORAGE_BUFFERS_PER_STAGE}`,
      );
    }
    if (p.kind === "compute") {
      if (p.entry === "") throw err(`pipeline ${i} has no entry point`);
      const ws = p.workgroup_size;
      const ok =
        ws.every((s, k) => s >= 1 && s <= MAX_WORKGROUP_SIZE[k]!) &&
        ws[0] * ws[1] * ws[2] <= MAX_WORKGROUP_INVOCATIONS;
      if (!ok) throw err(`pipeline ${i}'s workgroup size [${ws.join(", ")}] is outside WebGPU's limits`);
    } else if (p.vertex_entry === "" || p.fragment_entry === "") {
      throw err(`pipeline ${i} is missing an entry point`);
    }
  });
}
