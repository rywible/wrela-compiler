// The manifest: parsing and validation. Mirrors `Manifest::parse` and
// `Manifest::validate` in runtime/abi/src/manifest.rs: the same version rules, the same reading
// of each field, the same checks in the same order, the same messages. (Only JSON that doesn't
// parse is reported in each host's own words.)

import {
  BINDING_KIND_TRAITS,
  BINDING_KINDS,
  COMPARES,
  MANIFEST_VERSION,
  MAX_UNIFORM_BUFFER_BINDING_SIZE,
  MAX_WORKGROUP_INVOCATIONS,
  MAX_WORKGROUP_SIZE,
  STAGE_LIMITS,
  STREAM_VERSION,
  TEXTURE_FORMATS,
} from "./abi.gen.ts";
import type { TextureFormat } from "./stream.ts";
import { errorMessage } from "./errors.ts";

export type UniformSpace = "uniform" | "storage";
/** How two depths compare, in WebGPU's names. */
export type Compare = Exclude<(typeof COMPARES)[number], null>;
/** What a binding binds: a storage buffer read or read-write, a texture, or a sampler. */
export type BindingKind = (typeof BINDING_KINDS)[number];

const traitsOf = (k: BindingKind) => BINDING_KIND_TRAITS.find((t) => t.name === k)!;
/** Whether a binding is a colour texture, which names its format. */
export const hasFormat = (k: BindingKind) => traitsOf(k).has_format;
/** Whether kernels write it: a storage texture. */
export const isStorage = (k: BindingKind) => traitsOf(k).is_storage;
/** Whether it's a 3D texture. */
export const is3d = (k: BindingKind) => traitsOf(k).is_3d;
/** The per-stage limit it counts against (`STAGE_LIMITS`). */
export const limitOf = (k: BindingKind) => traitsOf(k).limit;

export interface UniformBlock {
  binding: number;
  /** Bytes; every dispatch or draw of the pipeline carries exactly this many uniform bytes. */
  size: number;
  space: UniformSpace;
}

/** Which of a render pipeline's shaders reads a binding: both, if the manifest doesn't say. */
export type BindingStage = "both" | "vertex" | "fragment";

export interface ResourceBinding {
  binding: number;
  kind: BindingKind;
  stage: BindingStage;
  /** A colour texture's format: what its texels are read as, and a storage texture's format. */
  format: TextureFormat | null;
}

export type Stage =
  | { kind: "compute"; entry: string; workgroup_size: [number, number, number] }
  | {
      kind: "render";
      vertex_entry: string;
      fragment_entry: string;
      blend: boolean;
      /** Which triangles it drops by facing. */
      cull: Cull;
      depth_bias: DepthBias;
      depth: DepthState;
      /** Gives each fragment its own depth: drawn only in a pass with a depth target. */
      writes_depth: boolean;
      /** Its fragment shader returns a `u32`: drawn only into an `r32uint` target. */
      uint: boolean;
      /** Every combination of targets it's drawn into, first drawn first. */
      targets: RenderTarget[];
    };

/** What a render pipeline draws into: a colour target's format (null in a pass that draws only
 * depths; the screen's is `SCREEN_FORMAT`), and a depth target (`depth32float`) or none. */
export interface RenderTarget {
  color: TextureFormat | null;
  depth: boolean;
}

/** A target as errors name it: `rgba16float with depth`, `depth alone` (runtime/abi
 * `RenderTarget`'s `Display`). */
export const targetName = (t: RenderTarget) =>
  t.color === null ? "depth alone" : t.depth ? `${t.color} with depth` : t.color;

export type Cull = "none" | "front" | "back";

/** WebGPU's `depthBias`, `depthBiasSlopeScale` and `depthBiasClamp`. */
export interface DepthBias {
  constant: number;
  slope_scale: number;
  clamp: number;
}

/** WebGPU's `depthCompare` and `depthWriteEnabled`: by default, the nearest fragment is kept. */
export interface DepthState {
  compare: Compare;
  write: boolean;
}

export type Pipeline = Stage & {
  name: string;
  shader: string;
  uniform: UniformBlock | null;
  bindings: ResourceBinding[];
  /** A debug build's: the binding of the flag its bounds checks set (the native host's
   * runtime/abi/src/manifest.rs says what it holds). */
  debug_flag: number | null;
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

/** `oneOf`, or `fallback` where the key is missing. */
function oneOfOr<T extends string, F extends string>(o: Json, key: string, where: string, options: readonly T[], fallback: F): T | F {
  return o[key] === undefined ? fallback : oneOf(o, key, where, options);
}

/** A render pipeline's `targets`: each a colour format or null, and whether it has depth. */
function renderTargets(o: Json, where: string): RenderTarget[] {
  const ts = o["targets"];
  if (!Array.isArray(ts)) throw new ManifestError(`${where}.targets must be an array`);
  const names = TEXTURE_FORMATS.map((t) => t.name);
  return ts.map((v: unknown, k) => {
    const tw = `${where}.targets[${k}]`;
    const to = object(v, tw);
    if (!("color" in to)) throw new ManifestError(`${tw}.color must be a format or null`);
    const color = to["color"] === null ? null : (oneOf(to, "color", tw, names) as TextureFormat);
    if (typeof to["depth"] !== "boolean") throw new ManifestError(`${tw}.depth must be true or false`);
    return { color, depth: to["depth"] };
  });
}

/** `true` or `false`, or `fallback` where the key is missing. */
function flag(o: Json, key: string, where: string, fallback: boolean): boolean {
  const v = o[key] === undefined ? fallback : o[key];
  if (typeof v !== "boolean") throw new ManifestError(`${where}.${key} must be true or false`);
  return v;
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
  const list = field(o, "bindings", where);
  if (!Array.isArray(list)) throw new ManifestError(`${where}.bindings must be an array`);
  const bindings = list.map((b, j) => {
    const bw = `${where}.bindings[${j}]`;
    const bo = object(b, bw);
    // A missing `stage` is both.
    const stage: BindingStage = oneOfOr(bo, "stage", bw, ["vertex", "fragment"] as const, "both");
    const kind = oneOf(bo, "kind", bw, BINDING_KINDS);
    // A colour texture's format.
    let format: TextureFormat | null = null;
    if (!hasFormat(kind)) {
      if (bo["format"] !== undefined) throw new ManifestError(`${bw}: only a colour texture has a \`format\``);
    } else {
      const f = TEXTURE_FORMATS.find((t) => t.name === oneOf(bo, "format", bw, TEXTURE_FORMATS.map((t) => t.name)))!;
      if (f.depth) throw new ManifestError(`${bw}: a colour texture's format isn't a depth one`);
      if (isStorage(kind) && !f.storable) {
        throw new ManifestError(`${bw}: kernels can't write ${f.name} textures`);
      }
      format = f.name;
    }
    return { binding: u32(bo, "binding", bw), kind, stage, format };
  });
  const kind = oneOf(o, "kind", where, ["compute", "render"] as const);
  let stage: Stage;
  if (kind === "compute") {
    const ws = field(o, "workgroup_size", where);
    if (!Array.isArray(ws) || ws.length !== 3) {
      throw new ManifestError(`${where}.workgroup_size must be an array of 3 u32s`);
    }
    const sizes = ws.map((s: unknown, k) => asU32(s, `${where}.workgroup_size[${k}]`));
    stage = { kind, entry: str(o, "entry", where), workgroup_size: [sizes[0]!, sizes[1]!, sizes[2]!] };
  } else {
    const vertex_entry = str(o, "vertex_entry", where);
    const fragment_entry = str(o, "fragment_entry", where);
    // A missing `blend` is false.
    const b = flag(o, "blend", where, false);
    // A missing `cull` is none, and a missing `depth_bias` none.
    const cull = oneOfOr(o, "cull", where, ["none", "front", "back"] as const, "none");
    let depth_bias: DepthBias = { constant: 0, slope_scale: 0, clamp: 0 };
    if (o["depth_bias"] !== undefined) {
      const bw = `${where}.depth_bias`;
      const bo = object(o["depth_bias"], bw);
      const c = field(bo, "constant", bw);
      if (typeof c !== "number" || !Number.isInteger(c) || c < -0x8000_0000 || c > 0x7fff_ffff) {
        throw new ManifestError(`${bw}.constant must be an i32`);
      }
      const finite = (key: string) => {
        const x = field(bo, key, bw);
        if (typeof x !== "number" || !Number.isFinite(Math.fround(x))) throw new ManifestError(`${bw}.${key} must be a finite number`);
        return Math.fround(x);
      };
      depth_bias = { constant: c, slope_scale: finite("slope_scale"), clamp: finite("clamp") };
    }
    // A missing `depth` is the default; so are its missing fields.
    let depth: DepthState = { compare: "less", write: true };
    if (o["depth"] !== undefined) {
      const dw = `${where}.depth`;
      const d = object(o["depth"], dw);
      const compare = oneOfOr(d, "compare", dw, COMPARES.filter((c): c is Compare => c !== null), "less");
      depth = { compare, write: flag(d, "write", dw, true) };
    }
    // A missing `writes_depth` is false, and a missing `uint`.
    const writes_depth = flag(o, "writes_depth", where, false);
    const uint = flag(o, "uint", where, false);
    stage = { kind, vertex_entry, fragment_entry, blend: b, cull, depth_bias, depth, writes_depth, uint, targets: renderTargets(o, where) };
  }
  // A missing `debug_flag` is read as null.
  const d = o["debug_flag"] ?? null;
  const debug_flag = d === null ? null : asU32(d, `${where}.debug_flag`);
  return { ...stage, name, shader, uniform, bindings, debug_flag };
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
    const bindings = p.bindings.map((b) => b.binding);
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
    if (p.debug_flag !== null) bindings.push(p.debug_flag);
    if (new Set(bindings).size !== bindings.length) throw err(`pipeline ${i} uses a binding twice`);
    const render = p.kind === "render";
    for (const b of p.bindings) {
      if (!render && b.stage !== "both") {
        throw err(`pipeline ${i}'s binding ${b.binding} names a stage, but it's a kernel's`);
      }
      if (b.stage === "vertex" && b.kind === "read_write") {
        throw err(`pipeline ${i}'s binding ${b.binding} is written, and a vertex shader can't write`);
      }
    }
    // What each stage binds, against WebGPU's per-stage limits: its bindings, and among its
    // storage buffers the uniform block's when it's in storage, and the debug flag (a fragment
    // shader's or a kernel's).
    const stages: BindingStage[] = render ? ["vertex", "fragment"] : ["both"];
    for (const stage of stages) {
      for (const limit of STAGE_LIMITS) {
        let n = p.bindings.filter((b) => limitOf(b.kind) === limit.name && (b.stage === "both" || b.stage === stage)).length;
        if (limit.name === "storage buffers") {
          n += (u?.space === "storage" ? 1 : 0) + (p.debug_flag !== null && stage !== "vertex" ? 1 : 0);
        }
        if (n > limit.max) {
          const what = stage === "vertex" ? " in its vertex shader" : stage === "fragment" ? " in its fragment shader" : "";
          throw err(`pipeline ${i} has ${n} ${limit.name}${what}; WebGPU's default limit is ${limit.max}`);
        }
      }
    }
    if (p.kind === "compute") {
      if (p.entry === "") throw err(`pipeline ${i} has no entry point`);
      const ws = p.workgroup_size;
      const ok =
        ws.every((s, k) => s >= 1 && s <= MAX_WORKGROUP_SIZE[k]!) &&
        ws[0] * ws[1] * ws[2] <= MAX_WORKGROUP_INVOCATIONS;
      if (!ok) throw err(`pipeline ${i}'s workgroup size [${ws.join(", ")}] is outside WebGPU's limits`);
    } else {
      if (p.vertex_entry === "" || p.fragment_entry === "") throw err(`pipeline ${i} is missing an entry point`);
      if (p.targets.length === 0) throw err(`pipeline ${i} names no targets`);
      p.targets.forEach((t, k) => {
        const name = targetName(t);
        if (p.targets.slice(0, k).some((s) => s.color === t.color && s.depth === t.depth)) {
          throw err(`pipeline ${i} names the target ${name} twice`);
        }
        const format = TEXTURE_FORMATS.find((f) => f.name === t.color);
        const integer = format?.uint ?? false;
        const why =
          p.writes_depth && !t.depth
            ? "its fragments give their depth, so a target has depth"
            : p.uint && t.color !== null && !integer
              ? "its fragment shader returns a `u32`, so a colour target is r32uint"
              : !p.uint && integer
                ? "its fragment shader returns a colour, which an r32uint target isn't"
                : format?.depth
                  ? "a colour target's format isn't a depth one"
                  : null;
        if (why !== null) throw err(`pipeline ${i}'s target ${name} can't be one: ${why}`);
      });
    }
  });
}
