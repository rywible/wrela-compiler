// Stages a page: a build's files (manifest.json, its WASM and WGSL) next to the runtime from
// dist/, plus an empty results/ directory, ready for tools/headless.py or tools/serve.py. This is
// the packaging step `wrela build` will do; it copies, so the runtime's sources live in one place.
//
//     bun scripts/stage.ts <build-dir> <page-dir>

import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { parseManifest } from "../src/manifest.ts";
import { ROOT } from "./build.ts";

/** Stages `buildDir` into `pageDir`; returns the files copied. */
export function stage(buildDir: string, pageDir: string): string[] {
  const manifest = parseManifest(readFileSync(join(buildDir, "manifest.json"), "utf8"));
  const dist = join(ROOT, "dist");
  if (!existsSync(join(dist, "index.html"))) throw new Error(`${dist} has no runtime; run \`bun run build\``);
  const files: [string, string][] = [
    ...readdirSync(dist).map((name): [string, string] => [join(dist, name), name]),
    ...["manifest.json", manifest.wasm, ...manifest.pipelines.map((p) => p.shader)].map(
      (name): [string, string] => [join(buildDir, name), name],
    ),
  ];
  for (const [from, name] of files) {
    const to = join(pageDir, name);
    mkdirSync(dirname(to), { recursive: true });
    copyFileSync(from, to);
  }
  // Results of an earlier run mustn't pass for this one's.
  const results = join(pageDir, "results");
  rmSync(results, { recursive: true, force: true });
  mkdirSync(results, { recursive: true });
  return files.map(([, name]) => name);
}

if (import.meta.main) {
  const [buildDir, pageDir] = process.argv.slice(2);
  if (!buildDir || !pageDir) {
    console.error("usage: bun scripts/stage.ts <build-dir> <page-dir>");
    process.exit(2);
  }
  const files = stage(resolve(buildDir), resolve(pageDir));
  console.log(`staged ${files.join(", ")} into ${resolve(pageDir)}`);
}
