import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { type Manifest, ManifestError, parseManifest } from "../src/manifest.ts";

// Versions and the pipeline checks, with their messages, are in the ABI's vectors
// (vectors.test.ts); these are what only this parser does.

/** The Rust crate's golden manifest (runtime/abi/src/manifest.rs, `golden_json`). */
const GOLDEN = `{
  "manifest_version": 1,
  "stream_version": 2,
  "wasm": "game.wasm",
  "pipelines": [
    {
      "name": "sample",
      "shader": "pipeline_0.wgsl",
      "kind": "compute",
      "entry": "main",
      "workgroup_size": [64, 1, 1],
      "uniform": { "binding": 0, "size": 32, "space": "uniform" },
      "buffers": [{ "binding": 1, "access": "read_write" }]
    },
    {
      "name": "cover+shade",
      "shader": "pipeline_1.wgsl",
      "kind": "render",
      "vertex_entry": "vs",
      "fragment_entry": "fs",
      "uniform": null,
      "buffers": []
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
    manifest_version: 1,
    stream_version: 2,
    wasm: "game.wasm",
    pipelines: [
      {
        kind: "compute",
        name: "sample",
        shader: "pipeline_0.wgsl",
        entry: "main",
        workgroup_size: [64, 1, 1],
        uniform: { binding: 0, size: 32, space: "uniform" },
        buffers: [{ binding: 1, access: "read_write" }],
      },
      {
        kind: "render",
        name: "cover+shade",
        shader: "pipeline_1.wgsl",
        vertex_entry: "vs",
        fragment_entry: "fs",
        uniform: null,
        buffers: [],
      },
    ],
  });
});

test("parses the first-light fixture", () => {
  const json = readFileSync(join(import.meta.dir, "../../fixtures/first-light/manifest.json"), "utf8");
  const m = parseManifest(json);
  expect(m.pipelines.map((p) => [p.name, p.kind])).toEqual([
    ["first-light", "render"],
    ["fill", "compute"],
  ]);
});

test("rejects malformed JSON and wrong field types", () => {
  expect(() => parseManifest("{")).toThrow(ManifestError);
  expectInvalid(() => parseManifest(GOLDEN.replace('"binding": 1', '"binding": -1')), "pipelines[0].buffers[0].binding must be a u32, not -1");
  expectInvalid(() => parseManifest(GOLDEN.replace('"kind": "render"', '"kind": "raster"')), "pipelines[1].kind must be one of compute, render");
  expectInvalid(() => parseManifest(GOLDEN.replace('"wasm": "game.wasm",', "")), "missing field `wasm` in the manifest");
});

test("a missing uniform is no uniform block, as in serde", () => {
  const m = parseManifest(GOLDEN.replace('"uniform": null,', ""));
  expect(m.pipelines[1]!.uniform).toBeNull();
});
