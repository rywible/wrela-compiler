// Reports the runtime's shipped size (every file in dist/) and fails above the budget: 1 MB
// (D-100), counted as 1,000,000 bytes, uncompressed. Gzipped sizes are shown for reference.
//
//     bun scripts/size.ts

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { ROOT } from "./build.ts";

const BUDGET = 1_000_000;
const dist = join(ROOT, "dist");
const row = (bytes: number, gzipped: number, name: string) =>
  console.log(`${String(bytes).padStart(9)}  ${String(gzipped).padStart(9)}  ${name}`);

console.log(`${"bytes".padStart(9)}  ${"gzipped".padStart(9)}`);
let total = 0;
let totalGzipped = 0;
for (const name of readdirSync(dist).sort()) {
  const bytes = readFileSync(join(dist, name));
  const gzipped = Bun.gzipSync(bytes).length;
  total += bytes.length;
  totalGzipped += gzipped;
  row(bytes.length, gzipped, name);
}
row(total, totalGzipped, `total (budget ${BUDGET} bytes)`);
if (total > BUDGET) {
  console.error(`the runtime is ${total} bytes, over its ${BUDGET}-byte budget`);
  process.exit(1);
}
