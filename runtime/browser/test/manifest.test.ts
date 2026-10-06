import { describe, expect, test } from "bun:test";
import { type Manifest, ManifestError, parseManifest } from "../src/manifest.ts";
import { readFixtureText } from "./fixtures.ts";

// Versions and the pipeline checks, with their messages, are in the ABI's vectors
// (vectors.test.ts); these are what only this parser does.

/** The Rust crate's golden manifest (runtime/abi/src/manifest.rs, `golden_json`). */
const GOLDEN = `{
  "manifest_version": 4,
  "stream_version": 6,
  "wasm": "game.wasm",
  "pipelines": [
    {
      "name": "sample",
      "shader": "pipeline_0.wgsl",
      "kind": "compute",
      "entry": "main",
      "workgroup_size": [64, 1, 1],
      "uniform": { "binding": 0, "size": 32, "space": "uniform" },
      "bindings": [{ "binding": 1, "kind": "read_write" }]
    },
    {
      "name": "cover+shade",
      "shader": "pipeline_1.wgsl",
      "kind": "render",
      "vertex_entry": "vs",
      "fragment_entry": "fs",
      "uniform": null,
      "bindings": [{ "binding": 0, "kind": "texture" }, { "binding": 1, "kind": "sampler" }]
    }
  ]
}`;

const sample = (): Manifest => parseManifest(GOLDEN);

function expectInvalid(f: () => unknown, text: string): void {
  expect(f).toThrow(ManifestError);
  expect(f).toThrow(`invalid manifest: ${text}`);
}

test("parses the Rust crate's golden manifest", () => {
  expect(sample()).toEqual({
    manifest_version: 4,
    stream_version: 6,
    wasm: "game.wasm",
    pipelines: [
      {
        kind: "compute",
        name: "sample",
        shader: "pipeline_0.wgsl",
        entry: "main",
        workgroup_size: [64, 1, 1],
        uniform: { binding: 0, size: 32, space: "uniform" },
        bindings: [{ binding: 1, kind: "read_write" }],
        debug_flag: null,
      },
      {
        kind: "render",
        name: "cover+shade",
        shader: "pipeline_1.wgsl",
        vertex_entry: "vs",
        fragment_entry: "fs",
        blend: false,
        cull: "none",
        depth_bias: { constant: 0, slope_scale: 0, clamp: 0 },
        uniform: null,
        bindings: [
          { binding: 0, kind: "texture" },
          { binding: 1, kind: "sampler" },
        ],
        debug_flag: null,
      },
    ],
  });
});

test("parses the first-light fixture", () => {
  const m = parseManifest(readFixtureText("manifest.json"));
  expect(m.pipelines.map((p) => [p.name, p.kind])).toEqual([
    ["first-light", "render"],
    ["fill", "compute"],
  ]);
});

test("rejects malformed JSON and wrong field types", () => {
  expect(() => parseManifest("{")).toThrow(ManifestError);
  expectInvalid(() => parseManifest(GOLDEN.replace('"binding": 1', '"binding": -1')), "pipelines[0].bindings[0].binding must be a u32, not -1");
  expectInvalid(() => parseManifest(GOLDEN.replace('"kind": "render"', '"kind": "raster"')), "pipelines[1].kind must be one of compute, render");
  expectInvalid(() => parseManifest(GOLDEN.replace('"wasm": "game.wasm",', "")), "missing field `wasm` in the manifest");
});

test("a missing uniform is no uniform block, as in serde", () => {
  const m = parseManifest(GOLDEN.replace('"uniform": null,', ""));
  expect(m.pipelines[1]!.uniform).toBeNull();
});
