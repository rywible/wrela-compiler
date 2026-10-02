// The manifest, version 1: parsing and validation. Mirrors `Manifest::parse` and
// `Manifest::validate` in runtime/abi/src/manifest.rs: the same version rules, the same checks
// in the same order, the same messages. (Malformed JSON and wrong field types are reported in
// this file's own words, not serde's. JSON can't tell `1` from `1.0`, so a whole number written
// with a fraction is accepted here where serde rejects it.)

import {
  MANIFEST_VERSION,
  MAX_STORAGE_BUFFERS_PER_STAGE,
  MAX_WORKGROUP_INVOCATIONS,
  MAX_WORKGROUP_SIZE,
  STREAM_VERSION,
} from "./abi.gen.ts";

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

const isObject = (v: unknown): v is Json => typeof v === "object" && v !== null && !Array.isArray(v);

function field(o: Json, key: string, where: string): unknown {
  if (!(key in o)) throw new ManifestError(`missing field \`${key}\` in ${where}`);
  return o[key];
}

function asU32(v: unknown, what: string): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0 || v > 0xffff_ffff) {
    throw new ManifestError(`${what} must be a u32, not ${JSON.stringify(v)}`);
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
    throw new ManifestError(`${where}.${key} must be one of ${options.join(", ")}, not ${JSON.stringify(v)}`);
  }
  return v as T;
}

function pipeline(v: unknown, i: number): Pipeline {
  const where = `pipelines[${i}]`;
  const o = object(v, where);
  const name = str(o, "name", where);
  const shader = str(o, "shader", where);
  // serde reads a missing `uniform` as None, like null.
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

/** Parses and validates a manifest; any other version is rejected. */
export function parseManifest(json: string): Manifest {
  let v: unknown;
  try {
    v = JSON.parse(json);
  } catch (e) {
    throw new ManifestError(e instanceof Error ? e.message : String(e));
  }
  const version = isObject(v) ? v["manifest_version"] : undefined;
  if (version === undefined) {
    throw new ManifestError(`no manifest_version; this host reads version ${MANIFEST_VERSION}`);
  }
  if (version !== MANIFEST_VERSION) {
    throw new ManifestError(
      `manifest version ${JSON.stringify(version)}, but this host reads version ${MANIFEST_VERSION}`,
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

export function validateManifest(m: Manifest): void {
  const err = (why: string) => new ManifestError(why);
  if (m.manifest_version !== MANIFEST_VERSION) {
    throw err(`manifest version ${m.manifest_version}, expected ${MANIFEST_VERSION}`);
  }
  if (m.stream_version !== STREAM_VERSION) {
    throw err(`command stream version ${m.stream_version}, but this host reads ${STREAM_VERSION}`);
  }
  if (m.wasm === "") throw err("no WASM file");
  m.pipelines.forEach((p, i) => {
    if (p.shader === "") throw err(`pipeline ${i} has no shader`);
    const bindings = p.buffers.map((b) => b.binding);
    const u = p.uniform;
    if (u !== null) {
      if (u.size === 0 || u.size % 4 !== 0) {
        throw err(`pipeline ${i}'s uniform size ${u.size} isn't a positive multiple of 4`);
      }
      if (u.space === "uniform" && u.size % 16 !== 0) {
        throw err(`pipeline ${i}'s uniform block of ${u.size} bytes isn't a multiple of 16`);
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
