// What the tests stand in for Cloudflare with: a Durable Object's storage on bun:sqlite, the
// modules Wrangler gives a bundle, a namespace of objects one an instance, a Worker Loader and an
// R2 bucket, and a WebSocket's server end.

import { plugin } from "bun";
import { Database } from "bun:sqlite";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Bucket, Loader, ServerStub, WorkerCode } from "../src/cloudflare.ts";
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
          WorkerEntrypoint: class {
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

/** An object of the namespace: its RPC methods, and its alarm. */
type ServerObject = ServerStub & { alarm(): Promise<void> };

/** A Durable Object namespace of `WrelaServer`s, one object (with its own bun:sqlite database)
 * an instance name, made the first time it's named, as `getByName` makes one. `restart(name)`
 * resets one, as an eviction or a crash does: a new object over the same storage. Each RPC goes
 * through `wire`, which a test may hold, repeat or reorder. */
export class Namespace {
  readonly objects = new Map<string, ServerObject>();
  readonly dbs = new Map<string, Database>();
  readonly alarms = new Map<string, number>();
  /** Every RPC passes through: `go` makes it. */
  wire: <T>(rpc: { method: string; name: string; args: unknown[] }, go: () => Promise<T>) => Promise<T> = (_rpc, go) => go();

  constructor(
    readonly make: (ctx: unknown) => ServerObject,
  ) {}

  /** Instance `name`'s object, made if it isn't. */
  object(name: string): ServerObject {
    let o = this.objects.get(name);
    if (o === undefined) {
      if (!this.dbs.has(name)) this.dbs.set(name, new Database(":memory:"));
      o = this.make({ storage: storage(this.dbs.get(name)!, (at) => void this.alarms.set(name, at)) });
      this.objects.set(name, o);
    }
    return o;
  }

  /** Instance `name`'s object is reset: what it held in memory goes, and its storage stays. */
  restart(name: string): void {
    this.objects.delete(name);
  }

  getByName(name: string): ServerStub {
    const rpc = <T>(method: string, args: unknown[], go: () => Promise<T>) => this.wire({ method, name, args }, go);
    // Structured clone, as RPC copies its arguments.
    const copy = <T>(x: T): T => structuredClone(x);
    return {
      request: (n, bytes) => rpc("request", [n, bytes], () => this.object(name).request(n, copy(bytes))),
      call: (n, from, body) => rpc("call", [n, from, body], () => this.object(name).call(n, from, copy(body))),
      deliver: (n, from, id, body) => rpc("deliver", [n, from, id, body], () => this.object(name).deliver(n, from, id, copy(body))),
      answer: (service, body) => rpc("answer", [service, body], () => this.object(name).answer(service, copy(body))),
      commit: (batches) => rpc("commit", [batches], () => this.object(name).commit(copy(batches))),
    };
  }

  /** Instance `name`'s stored value of `key`, or null. */
  stored(name: string, key: string): Uint8Array | null {
    const row = this.dbs.get(name)?.query("SELECT value FROM kv WHERE key = ?").get(new TextEncoder().encode(key)) as { value: Uint8Array } | null | undefined;
    return row === null || row === undefined ? null : new Uint8Array(row.value);
  }

  /** How many messages instance `name`'s outbox holds. */
  waiting(name: string): number {
    const row = this.dbs.get(name)?.query("SELECT COUNT(*) AS n FROM wrela_outbox").get() as { n: number } | undefined;
    return row?.n ?? 0;
  }
}

/** An R2 bucket of files, by key. */
export function bucket(files: Record<string, Uint8Array | string>): Bucket {
  return {
    get: async (key) => {
      const f = files[key];
      if (f === undefined) return null;
      const bytes = typeof f === "string" ? new TextEncoder().encode(f) : f;
      return { text: async () => new TextDecoder().decode(bytes), arrayBuffer: async () => bytes.slice().buffer };
    },
  };
}

/** A Worker Loader: each new id's code is written to a directory of its own and its main module
 * imported (with `workers()`'s modules), as an isolate of its own would load it. What each load
 * was given is kept, so a test sees that it had no bindings and no network. */
export class FakeLoader implements Loader {
  readonly loaded = new Map<string, WorkerCode>();
  readonly #workers = new Map<string, Promise<{ default: new (ctx: unknown, env: unknown) => { verify(b: Uint8Array): Promise<Uint8Array | null> } }>>();

  get(id: string, code: () => Promise<WorkerCode>) {
    let worker = this.#workers.get(id);
    if (worker === undefined) {
      worker = code().then(async (c) => {
        this.loaded.set(id, c);
        const dir = mkdtempSync(join(tmpdir(), "wrela-loader-"));
        for (const [name, m] of Object.entries(c.modules)) {
          mkdirSync(join(dir, name, ".."), { recursive: true });
          writeFileSync(join(dir, name), typeof m === "string" ? m : new Uint8Array(m.wasm));
        }
        return import(join(dir, c.mainModule));
      });
      this.#workers.set(id, worker);
    }
    const w = worker;
    return {
      getEntrypoint: () => ({
        verify: async (bytes: Uint8Array) => {
          const Main = (await w).default;
          return new Main({}, {}).verify(structuredClone(bytes));
        },
      }),
    };
  }
}
