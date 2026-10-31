// A built server program through its Cloudflare host, as the Worker runs it: `api` in front, the
// Durable Object behind, its storage on bun:sqlite. The compiler's suite builds the program
// (compiler/tests/tests/suite/http.rs) and runs this with WRELA_HTTP_BUILD naming the build; run
// alone, it has nothing to test and skips.

import { Database } from "bun:sqlite";
import { plugin } from "bun";
import { beforeAll, describe, expect, test } from "bun:test";
import { join } from "node:path";
import type { ApiOptions, Env, ResponseCache } from "../src/cloudflare.ts";
import type { Storage } from "../src/host.ts";

const BUILD = process.env["WRELA_HTTP_BUILD"];
const SITE = "https://wrela.dev";

/** A Durable Object's storage on bun:sqlite: `exec`'s rows, `transactionSync`, and the alarms
 * asked for. */
function storage(db: Database, alarms: number[]): Storage & { setAlarm(at: number): Promise<void> } {
  return {
    sql: {
      exec(query: string, ...bindings: unknown[]) {
        const rows = db.query(query).all(...(bindings as never[])) as Record<string, unknown>[];
        return Object.assign(rows[Symbol.iterator](), { toArray: () => rows });
      },
    },
    transactionSync: <T>(fn: () => T): T => db.transaction(fn)(),
    setAlarm: async (at: number) => void alarms.push(at),
  };
}

interface Host {
  api(request: Request, env: Env, options?: ApiOptions): Promise<Response | null>;
  WrelaServer: new (ctx: unknown, env: Env) => { request(bytes: Uint8Array): Promise<Uint8Array> };
}

let host: Host;
let env: Env;
let db: Database;

/** The keys a rate limit counted, and how many it lets each have. */
const counted_keys: string[] = [];
const SIGN_INS = 2;

/** A cache that keeps what it's given, by URL. */
const kept = new Map<string, Response>();
const cache: ResponseCache = {
  match: async (r) => kept.get(r.url)?.clone(),
  put: async (r, response) => void kept.set(r.url, response),
};

beforeAll(async () => {
  if (BUILD === undefined) return;
  // What Wrangler gives the bundle: `cloudflare:workers`, and `game.wasm` compiled.
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
  host = (await import(join(BUILD, "server/cloudflare.js"))) as Host;
  db = new Database(":memory:");
  const object = { current: null as InstanceType<Host["WrelaServer"]> | null };
  env = {
    WRELA_ORIGINS: `${SITE}, https://other.wrela.dev`,
    WRELA_LIMITS: JSON.stringify([{ binding: "SIGN_IN", prefix: "/api/sign-in", methods: ["POST"], period: 10 }]),
    SIGN_IN: {
      limit: async ({ key }: { key: string }) => {
        counted_keys.push(key);
        return { success: counted_keys.filter((k) => k === key).length <= SIGN_INS };
      },
    },
    WRELA_SERVER: { getByName: () => ({ request: (bytes) => object.current!.request(bytes) }) },
  };
  object.current = new host.WrelaServer({ storage: storage(db, []) }, env);
});

/** The API's response to a request of `method` for `path`. */
async function ask(path: string, init: RequestInit = {}): Promise<Response> {
  const r = await host.api(new Request(`${SITE}${path}`, init), env, { cache });
  if (r === null) throw new Error(`${path} isn't the API's`);
  return r;
}

const stored = (key: string): Uint8Array | null => {
  const row = db.query("SELECT value FROM kv WHERE key = ?").get(new TextEncoder().encode(key)) as { value: Uint8Array } | null;
  return row === null ? null : new Uint8Array(row.value);
};

const counted = (...parts: Uint8Array[]): Uint8Array<ArrayBuffer> => {
  const out: number[] = [];
  for (const p of parts) {
    const n = new Uint8Array(new Uint32Array([p.length]).buffer);
    out.push(...n, ...p);
  }
  return new Uint8Array(out);
};

describe.skipIf(BUILD === undefined)("a server build over HTTP", () => {
  test("a read is answered on the next call, and the write commits before the response", async () => {
    const first = await ask("/api/count");
    expect(first.status).toBe(200);
    expect(await first.text()).toBe("1");
    expect(first.headers.get("set-cookie")).toBe("__Host-visit=1; Path=/; Max-Age=3600; Secure; HttpOnly; SameSite=Strict");
    expect(stored("count")).toEqual(new Uint8Array([1, 0, 0, 0]));
    const again = await ask("/api/count", { headers: { cookie: "__Host-visit=1" } });
    expect(await again.text()).toBe("2");
    expect(again.headers.get("set-cookie")).toBeNull();
    expect(stored("count")).toEqual(new Uint8Array([2, 0, 0, 0]));
    // The host's own headers.
    expect(again.headers.get("cache-control")).toBe("no-store");
    expect(again.headers.get("x-content-type-options")).toBe("nosniff");
    expect(again.headers.get("content-security-policy")).toBe("default-src 'none'");
  });

  test("the time of day, random bytes and a signature", async () => {
    const before = Date.now();
    const ms = Number(await (await ask("/api/time")).text());
    expect(ms).toBeGreaterThanOrEqual(before - 1);
    expect(ms).toBeLessThanOrEqual(Date.now() + 1);
    const a = new Uint8Array(await (await ask("/api/random")).arrayBuffer());
    const b = new Uint8Array(await (await ask("/api/random")).arrayBuffer());
    expect(a.length).toBe(16);
    expect(a).not.toEqual(b);
    const pair = (await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"])) as CryptoKeyPair;
    const key = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
    const message = new TextEncoder().encode("hello");
    const signature = new Uint8Array(await crypto.subtle.sign({ name: "Ed25519" }, pair.privateKey, message));
    const post = (m: Uint8Array) => ask("/api/verify", { method: "POST", headers: { origin: SITE }, body: counted(key, m, signature) });
    expect(await (await post(message)).text()).toBe("1");
    expect(await (await post(new TextEncoder().encode("hellO"))).text()).toBe("0");
  });

  test("a server sees its allowlisted headers, never an address", async () => {
    const r = await ask("/api/headers", {
      headers: {
        "cf-connecting-ip": "203.0.113.7",
        "x-forwarded-for": "203.0.113.7",
        "true-client-ip": "203.0.113.7",
        "x-real-ip": "203.0.113.7",
        "cf-ipcountry": "NZ",
        "user-agent": "a browser",
        cookie: "a=1",
        "x-wrela-csrf": "7f",
        accept: "text/plain",
      },
    });
    expect(await r.text()).toBe("accept,cookie,x-wrela-csrf,true");
  });

  test("a request's method, query and body cross, and a HEAD has no body", async () => {
    const r = await ask("/api/echo?x=1&y=2", { method: "PUT", headers: { origin: SITE }, body: new Uint8Array([7, 8, 9]) });
    expect(r.status).toBe(201);
    expect(r.headers.get("x-wrela-method")).toBe("PUT");
    expect(r.headers.get("x-wrela-query")).toBe("x=1&y=2");
    expect(new Uint8Array(await r.arrayBuffer())).toEqual(new Uint8Array([7, 8, 9]));
    const head = await ask("/api/echo", { method: "HEAD" });
    expect(head.status).toBe(201);
    expect((await head.arrayBuffer()).byteLength).toBe(0);
    expect((await ask("/api/nowhere")).status).toBe(404);
  });

  test("the router refuses before the object", async () => {
    expect(await host.api(new Request(`${SITE}/index.html`), env)).toBeNull();
    expect((await ask("/api/echo", { method: "POST", body: "x" })).status).toBe(403);
    expect((await ask("/api/echo", { method: "POST", headers: { origin: "https://evil.example" }, body: "x" })).status).toBe(403);
    expect((await ask("/api/echo", { method: "POST", headers: { origin: SITE, "sec-fetch-site": "cross-site" }, body: "x" })).status).toBe(403);
    expect((await ask("/api/echo", { method: "POST", headers: { origin: "https://other.wrela.dev" }, body: "x" })).status).toBe(201);
    const trace = await ask("/api/echo", { method: "TRACE" });
    expect(trace.status).toBe(405);
    expect(trace.headers.get("allow")).toContain("GET");
    expect((await ask("/api/echo", { method: "POST", headers: { origin: SITE }, body: new Uint8Array((1 << 20) + 1) })).status).toBe(413);
  });

  test("a trap fails the call, commits nothing, and the next request starts afresh", async () => {
    expect(Number(await (await ask("/api/closed")).text())).toBeGreaterThan(0);
    expect((await ask("/api/trap")).status).toBe(500);
    expect(stored("t")).toBeNull();
    expect(await (await ask("/api/closed")).text()).toBe("0");
    // A second response, or a header a server can't set, fails the call as well.
    expect((await ask("/api/twice")).status).toBe(500);
    expect((await ask("/api/bad-header")).status).toBe(500);
    expect(await (await ask("/api/count", { headers: { cookie: "__Host-visit=1" } })).text()).toBe("3");
  });

  test("a client past its rate limit gets 429, keyed on its address alone", async () => {
    const signIn = (ip: string) => ask("/api/sign-in", { method: "POST", headers: { origin: SITE, "cf-connecting-ip": ip }, body: "x" });
    expect((await signIn("203.0.113.7")).status).toBe(404);
    expect((await signIn("203.0.113.7")).status).toBe(404);
    const third = await signIn("203.0.113.7");
    expect(third.status).toBe(429);
    expect(third.headers.get("retry-after")).toBe("10");
    expect((await signIn("198.51.100.2")).status).toBe(404);
    // Other paths and methods aren't counted.
    expect((await ask("/api/sign-in", { headers: { "cf-connecting-ip": "203.0.113.7" } })).status).toBe(404);
    expect(counted_keys).toEqual(["203.0.113.7", "203.0.113.7", "203.0.113.7", "198.51.100.2"]);
  });

  test("a public answer is served from the cache without the object", async () => {
    const first = await ask("/api/public");
    expect(await first.text()).toBe("1");
    expect(await (await ask("/api/public", { headers: { cookie: "a=1" } })).text()).toBe("1");
    expect(await (await ask("/api/public?other")).text()).toBe("2");
    // An answer that isn't public isn't kept.
    expect(kept.has(`${SITE}/api/count`)).toBe(false);
  });

  test("requests at once are answered each its own", async () => {
    const paths = ["/api/echo?a", "/api/time", "/api/echo?b", "/api/random", "/api/echo?c"];
    const all = await Promise.all(paths.map((p) => ask(p)));
    expect(all.map((r) => r.status)).toEqual([201, 200, 201, 200, 201]);
    expect([0, 2, 4].map((i) => all[i]!.headers.get("x-wrela-query"))).toEqual(["a", "b", "c"]);
  });
});
