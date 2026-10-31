// Builds the host into a directory (default: dist/): cloudflare.js, which `wrela build` writes
// beside a server build's module. It imports `./game.wasm` and `cloudflare:workers`, which
// Wrangler resolves when it bundles the site's Worker.
// dist/ is checked in, so the compiler can embed the host without bun; `bun run check-dist`
// fails when it's stale.
//
//     bun scripts/build.ts [out-dir]

import { mkdirSync, readdirSync, renameSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";

export const ROOT = resolve(import.meta.dir, "..");

/** Builds into `outDir`, replacing what's there; returns the files written. */
export async function build(outDir: string): Promise<string[]> {
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });
  const result = await Bun.build({
    entrypoints: [join(ROOT, "src/entry.ts")],
    outdir: outDir,
    target: "browser",
    format: "esm",
    external: ["cloudflare:workers", "*.wasm"],
    naming: "[name].[ext]",
  });
  if (!result.success) {
    throw new AggregateError(result.logs, "the host failed to build");
  }
  renameSync(join(outDir, "entry.js"), join(outDir, "cloudflare.js"));
  return readdirSync(outDir).sort();
}

if (import.meta.main) {
  const out = resolve(process.argv[2] ?? join(ROOT, "dist"));
  const files = await build(out);
  console.log(`built ${files.join(", ")} into ${out}`);
}
