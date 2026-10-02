// Fails when dist/ isn't what the sources build to: rebuilds into a temporary directory and
// compares every file.
//
//     bun scripts/check-dist.ts

import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { build, ROOT } from "./build.ts";

const dist = join(ROOT, "dist");
const temp = mkdtempSync(join(tmpdir(), "wrela-dist-"));
try {
  const fresh = await build(temp);
  const current = existsSync(dist) ? readdirSync(dist).sort() : [];
  const stale = [...new Set([...fresh, ...current])].filter((name) => {
    const a = join(temp, name);
    const b = join(dist, name);
    return !existsSync(a) || !existsSync(b) || !readFileSync(a).equals(readFileSync(b));
  });
  if (stale.length > 0) {
    console.error(`runtime/browser/dist is stale (${stale.join(", ")}); run \`bun run build\``);
    process.exit(1);
  }
  console.log(`dist is current (${fresh.join(", ")})`);
} finally {
  rmSync(temp, { recursive: true, force: true });
}
