import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { type Manifest, ManifestError, parseManifest, validateManifest } from "../src/manifest.ts";

/** The Rust crate's golden manifest (runtime/abi/src/manifest.rs, `golden_json`). */
const GOLDEN = `{
  "manifest_version": 1,
  "stream_version": 1,
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
    stream_version: 1,
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

describe("rejects other versions", () => {
  test("manifest version", () => {
    expectInvalid(() => parseManifest(GOLDEN.replace('"manifest_version": 1', '"manifest_version": 2')), "manifest version 2, but this host reads version 1");
    expectInvalid(() => parseManifest(GOLDEN.replace('"manifest_version": 1,', "")), "no manifest_version; this host reads version 1");
    expectInvalid(() => parseManifest("[]"), "no manifest_version");
  });
  test("command stream version", () => {
    expectInvalid(() => parseManifest(GOLDEN.replace('"stream_version": 1', '"stream_version": 9')), "command stream version 9, but this host reads 1");
  });
});

describe("rejects bad pipelines, with the Rust crate's messages", () => {
  const edit = (f: (m: Manifest) => void) => () => {
    const m = sample();
    f(m);
    validateManifest(m);
  };
  test("workgroup size", () => {
    expectInvalid(
      edit((m) => {
        const p = m.pipelines[0]!;
        if (p.kind === "compute") p.workgroup_size = [512, 1, 1];
      }),
      "pipeline 0's workgroup size [512, 1, 1] is outside WebGPU's limits",
    );
    expectInvalid(
      edit((m) => {
        const p = m.pipelines[0]!;
        if (p.kind === "compute") p.workgroup_size = [16, 16, 2];
      }),
      "pipeline 0's workgroup size [16, 16, 2] is outside WebGPU's limits",
    );
  });
  test("a binding used twice", () => {
    expectInvalid(edit((m) => (m.pipelines[0]!.buffers[0]!.binding = 0)), "pipeline 0 uses a binding twice");
  });
  test("uniform sizes", () => {
    expectInvalid(edit((m) => (m.pipelines[0]!.uniform = { binding: 0, size: 20, space: "uniform" })), "pipeline 0's uniform block of 20 bytes isn't a multiple of 16");
    expectInvalid(edit((m) => (m.pipelines[0]!.uniform = { binding: 0, size: 6, space: "storage" })), "pipeline 0's uniform size 6 isn't a positive multiple of 4");
    edit((m) => (m.pipelines[0]!.uniform = { binding: 0, size: 20, space: "storage" }))();
  });
  test("too many storage buffers", () => {
    expectInvalid(
      edit((m) => {
        m.pipelines[1]!.buffers = Array.from({ length: 9 }, (_, i) => ({ binding: i, access: "read" as const }));
      }),
      "pipeline 1 has 9 storage buffers; WebGPU's default limit is 8",
    );
  });
  test("missing shader or entry points", () => {
    expectInvalid(edit((m) => (m.pipelines[1]!.shader = "")), "pipeline 1 has no shader");
    expectInvalid(
      edit((m) => {
        const p = m.pipelines[1]!;
        if (p.kind === "render") p.fragment_entry = "";
      }),
      "pipeline 1 is missing an entry point",
    );
    expectInvalid(edit((m) => (m.wasm = "")), "no WASM file");
  });
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
