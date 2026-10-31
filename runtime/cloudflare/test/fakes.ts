// What the tests stand in for Cloudflare with: a Durable Object's storage on bun:sqlite, the
// modules Wrangler gives a bundle, and a WebSocket's server end.

import { plugin } from "bun";
import { Database } from "bun:sqlite";
import type { Storage } from "../src/host.ts";

/** A Durable Object's storage on bun:sqlite: `exec`'s rows, `transactionSync`, and `setAlarm`.
 * The SQLite storage API binds a BLOB as an ArrayBuffer; bun:sqlite takes a Uint8Array (an
 * ArrayBuffer binds as NULL there), so each is converted. */
export function storage(db = new Database(":memory:"), setAlarm: (at: number) => void = () => {}): Storage & { setAlarm(at: number): Promise<void> } {
  return {
    sql: {
      exec(query: string, ...bindings: unknown[]) {
        const bound = bindings.map((b) => (b instanceof ArrayBuffer ? new Uint8Array(b) : b));
        const rows = db.query(query).all(...(bound as never[])) as Record<string, unknown>[];
        return Object.assign(rows[Symbol.iterator](), { toArray: () => rows });
      },
    },
    transactionSync: <T>(fn: () => T): T => db.transaction(fn)(),
    setAlarm: async (at: number) => setAlarm(at),
  };
}

/** What Wrangler gives the bundle: `cloudflare:workers`, and `game.wasm` compiled. */
export function workers(): void {
  plugin({
    name: "workers",
    setup(build) {
      build.module("cloudflare:workers", () => ({
        exports: {
          DurableObject: class {
            constructor(
              readonly ctx: unknown,
              readonly env: unknown,
            ) {}
          },
        },
        loader: "object",
      }));
      build.onLoad({ filter: /\.wasm$/ }, async (args) => ({
        exports: { default: new WebAssembly.Module(await Bun.file(args.path).arrayBuffer()) },
        loader: "object",
      }));
    },
  });
}

/** A WebSocket's server end: what it was sent, its close code, and its attachment, kept as the
 * Hibernation API keeps it (a copy). */
export class FakeSocket {
  sent: Uint8Array[] = [];
  closed: number | null = null;
  attachment: unknown = null;
  send(bytes: Uint8Array) {
    this.sent.push(new Uint8Array(bytes));
  }
  close(code = 1000) {
    this.closed ??= code;
  }
  serializeAttachment(v: unknown) {
    this.attachment = structuredClone(v);
  }
  deserializeAttachment() {
    return this.attachment;
  }
}
