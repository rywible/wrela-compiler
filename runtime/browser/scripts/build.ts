// Builds the runtime into a directory (default: dist/): main.js, worker.js, worklet.js and
// index.html.
// dist/ is checked in, so the compiler can embed the runtime without bun; `bun run check-dist`
// fails when it's stale.
//
//     bun scripts/build.ts [out-dir]

import { copyFileSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";

export const ROOT = resolve(import.meta.dir, "..");

/** Builds into `outDir`, replacing what's there; returns the files written. */
export async function build(outDir: string): Promise<string[]> {
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });
  const result = await Bun.build({
    entrypoints: [join(ROOT, "src/main.ts"), join(ROOT, "src/worker.ts"), join(ROOT, "src/worklet.ts")],
    outdir: outDir,
    target: "browser",
    format: "esm",
    minify: true,
    naming: "[name].[ext]",
  });
  if (!result.success) {
    throw new AggregateError(result.logs, "the runtime failed to build");
  }
  copyFileSync(join(ROOT, "src/index.html"), join(outDir, "index.html"));
  return readdirSync(outDir).sort();
}

if (import.meta.main) {
  const out = resolve(process.argv[2] ?? join(ROOT, "dist"));
  const files = await build(out);
  console.log(`built ${files.join(", ")} into ${out}`);
}
